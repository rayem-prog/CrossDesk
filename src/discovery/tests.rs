use super::*;
#[test]
fn device_addresses_update_and_manual_disconnect_cannot_be_reversed_by_discovery() {
    let now = Instant::now();
    let mut catalog = Catalog::default();
    let mut p = Candidate {
        id: [3; 16],
        name: "Windows".into(),
        endpoints: vec!["192.168.1.10:24920".parse().unwrap()],
        pairing_port: Some(24921),
        protocol: crate::protocol::VERSION,
    };
    catalog
        .upsert(
            "service".into(),
            p.clone(),
            Some(now + Duration::from_secs(20)),
        )
        .unwrap();
    p.endpoints = vec!["192.168.1.114:24920".parse().unwrap()];
    catalog
        .upsert(
            "service".into(),
            p.clone(),
            Some(now + Duration::from_secs(20)),
        )
        .unwrap();
    assert_eq!(catalog.list().len(), 1);
    assert_eq!(
        catalog.list()[0].endpoints[0].to_string(),
        "192.168.1.114:24920"
    );
    catalog.expire(now + Duration::from_secs(20));
    assert!(catalog.list().is_empty());
    let mut policy = ConnectPolicy::new(Some([3; 16]), true);
    policy.manual_stop();
    assert!(!policy.should_connect([3; 16], now));
    policy.manual_connect();
    assert!(!policy.should_connect([4; 16], now));
    assert!(policy.should_connect([3; 16], now));
    policy.started();
    assert!(!policy.should_connect([3; 16], now));
}
#[test]
fn invalid_and_unbounded_discovery_records_are_rejected() {
    let mut c = Catalog::default();
    let now = Instant::now();
    for n in 0..32 {
        c.upsert(
            format!("service{n}"),
            Candidate {
                id: [n; 16],
                name: "Windows".into(),
                endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
                pairing_port: None,
                protocol: crate::protocol::VERSION,
            },
            Some(now + Duration::from_secs(20)),
        )
        .unwrap();
    }
    assert!(
        c.upsert(
            "overflow".into(),
            Candidate {
                id: [33; 16],
                name: "Windows".into(),
                endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
                pairing_port: None,
                protocol: crate::protocol::VERSION,
            },
            Some(now)
        )
        .is_err()
    );
    c.remove("service1");
    assert_eq!(c.list().len(), 31);
    assert!(
        c.upsert(
            "bad".into(),
            Candidate {
                id: [34; 16],
                name: "bad\nname".into(),
                endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
                pairing_port: None,
                protocol: crate::protocol::VERSION,
            },
            Some(now)
        )
        .is_err()
    );
}

#[test]
fn mdns_announcement_advertises_the_current_wire_protocol() {
    let observer = ServiceDaemon::new().unwrap();
    let events = observer.browse(TYPE).unwrap();
    let mut discovery = Discovery::browse().unwrap();
    let mut id = [0; 16];
    getrandom::getrandom(&mut id).unwrap();
    discovery.announce(id, "version-test", None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let protocol = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "mDNS announcement was not resolved");
        if let Ok(ServiceEvent::ServiceResolved(info)) = events.recv_timeout(remaining) {
            if info.get_property_val_str("id") == Some(hex(&id).as_str()) {
                break info.get_property_val_str("protocol").unwrap().to_owned();
            }
        }
    };
    discovery.stop();
    let _ = observer.shutdown();
    assert_eq!(protocol, crate::protocol::VERSION.to_string());
}

#[test]
fn discovery_keeps_legacy_and_current_protocol_devices_visible() {
    let mut discovery = Discovery::browse().unwrap();
    let publisher = ServiceDaemon::new().unwrap();
    let mut id = [0; 16];
    getrandom::getrandom(&mut id).unwrap();
    let ids = [id, {
        let mut other = id;
        other[0] ^= 1;
        other
    }];
    for (id, protocol) in ids.into_iter().zip(["6", "7"]) {
        let id = hex(&id);
        let properties = HashMap::from([
            ("id".to_owned(), id.clone()),
            ("role".to_owned(), "windows".to_owned()),
            ("protocol".to_owned(), protocol.to_owned()),
            ("name".to_owned(), format!("Windows-{protocol}")),
        ]);
        publisher
            .register(
                ServiceInfo::new(
                    TYPE,
                    &format!("Version-test-{id}"),
                    &format!("version-{id}.local."),
                    "",
                    24920,
                    properties,
                )
                .unwrap()
                .enable_addr_auto(),
            )
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let found = loop {
        let found: Vec<_> = discovery
            .snapshot()
            .into_iter()
            .filter(|p| ids.contains(&p.id))
            .collect();
        if found.len() == 2 || Instant::now() >= deadline {
            break found;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    discovery.stop();
    let _ = publisher.shutdown();
    assert_eq!(
        found.len(),
        2,
        "both legacy and current devices must be displayed"
    );
}

#[test]
fn duplicate_device_records_cannot_hide_a_protocol_mismatch() {
    for _ in 0..32 {
        let mut catalog = Catalog::default();
        for protocol in [7, 6] {
            catalog
                .upsert(
                    format!("instance-{protocol}"),
                    Candidate {
                        id: [9; 16],
                        name: "Windows".into(),
                        endpoints: vec!["127.0.0.1:24920".parse().unwrap()],
                        pairing_port: None,
                        protocol,
                    },
                    None,
                )
                .unwrap();
        }
        // HashMap iteration order must never decide whether an old peer is safe to connect.
        assert_ne!(catalog.list()[0].protocol, 7);
    }
}
