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
                pairing_port: None
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
                pairing_port: None
            },
            Some(now)
        )
        .is_err()
    );
}
