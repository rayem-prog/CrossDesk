//! Coordinates are relative to the complete Windows desktop on the wire;
//! monitor joins and negative monitor origins remain local implementation details.
#[derive(Debug, Clone, Copy)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub struct DesktopLayout {
    pub monitors: Vec<MonitorRect>,
    pub bounds: MonitorRect,
}

impl DesktopLayout {
    pub fn new(mut monitors: Vec<MonitorRect>) -> Self {
        monitors.retain(|r| r.right > r.left && r.bottom > r.top);
        if monitors.is_empty() {
            monitors.push(MonitorRect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            });
        }
        let bounds = MonitorRect {
            left: monitors.iter().map(|r| r.left).min().unwrap(),
            top: monitors.iter().map(|r| r.top).min().unwrap(),
            right: monitors.iter().map(|r| r.right).max().unwrap(),
            bottom: monitors.iter().map(|r| r.bottom).max().unwrap(),
        };
        Self { monitors, bounds }
    }
    pub fn size(&self) -> (i32, i32) {
        (
            self.bounds.right - self.bounds.left,
            self.bounds.bottom - self.bounds.top,
        )
    }

    pub fn local_point(&self, x: i32, y: i32) -> (i32, i32) {
        let row: Vec<_> = self
            .monitors
            .iter()
            .filter(|r| y >= r.top && y < r.bottom)
            .collect();
        let left = row.iter().map(|r| r.left).min().unwrap_or(self.bounds.left);
        let right = row
            .iter()
            .map(|r| r.right)
            .max()
            .unwrap_or(self.bounds.right)
            - 1;
        let local_x = if x <= left {
            0
        } else if x >= right {
            self.size().0 - 1
        } else {
            x - self.bounds.left
        };
        (local_x, (y - self.bounds.top).clamp(0, self.size().1 - 1))
    }

    pub fn return_point(&self, x: i32, y: i32) -> (i32, i32) {
        let desired_y = self.bounds.top + y.clamp(0, self.size().1 - 1);
        let nearest_y = self
            .monitors
            .iter()
            .map(|r| desired_y.clamp(r.top, r.bottom - 1))
            .min_by_key(|v| (*v as i64 - desired_y as i64).abs())
            .unwrap();
        let row: Vec<_> = self
            .monitors
            .iter()
            .filter(|r| nearest_y >= r.top && nearest_y < r.bottom)
            .collect();
        let left = row.iter().map(|r| r.left).min().unwrap();
        let right = row.iter().map(|r| r.right).max().unwrap() - 1;
        let absolute_x = if x < self.size().0 / 2 {
            left + x
        } else {
            right - (self.size().0 - 1 - x)
        };
        let nearest_x = row
            .iter()
            .map(|r| absolute_x.clamp(r.left, r.right - 1))
            .min_by_key(|v| (*v as i64 - absolute_x as i64).abs())
            .unwrap();
        (nearest_x, nearest_y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_origins_and_internal_joins_do_not_become_edges() {
        let d = DesktopLayout::new(vec![
            MonitorRect {
                left: -1920,
                top: -100,
                right: 0,
                bottom: 980,
            },
            MonitorRect {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1440,
            },
        ]);
        assert_eq!(d.size(), (4480, 1540));
        assert_eq!(d.local_point(-1920, 500), (0, 600));
        assert_eq!(d.local_point(-1, 500), (1919, 600));
        assert_eq!(d.local_point(0, 500), (1920, 600));
        assert_eq!(d.local_point(2559, 500), (4479, 600));
        assert_eq!(d.return_point(8, 600), (-1912, 500));
    }
    #[test]
    fn staggered_displays_use_the_visible_outer_edge_of_each_row() {
        let d = DesktopLayout::new(vec![
            MonitorRect {
                left: -1000,
                top: 0,
                right: 0,
                bottom: 600,
            },
            MonitorRect {
                left: 0,
                top: 0,
                right: 1200,
                bottom: 900,
            },
        ]);
        assert_eq!(d.local_point(0, 800), (0, 800));
        assert_eq!(d.return_point(8, 800), (8, 800));
        assert_eq!(d.return_point(2191, 800), (1191, 800));
    }
}
