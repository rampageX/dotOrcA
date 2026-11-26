use std::{sync::{Arc, atomic::{AtomicU64, Ordering}}, thread, time::{Duration, Instant}};

#[derive(Clone, Default)]
pub struct Worker(Arc<AtomicU64>);
// One atomic snapshot: 0 idle, 1..=4 progress, 5+elapsed*8 finished.
impl Worker {
    pub fn start(&self) {
        let current = self.0.load(Ordering::Acquire);
        if (1..=4).contains(&current) || current == u64::MAX { return; }
        if self.0.compare_exchange(current, u64::MAX, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
        let state = self.0.clone();
        thread::spawn(move || {
            let start = Instant::now();
            for i in 1..=5u64 {
                thread::sleep(Duration::from_secs(1));
                let value = if i == 5 { 5 + (start.elapsed().as_millis() as u64) * 8 } else { i };
                state.store(value, Ordering::Release);
            }
        });
    }
    pub fn running(&self) -> bool {
        let v = self.0.load(Ordering::Acquire);
        v == u64::MAX || (1..=4).contains(&v)
    }
    pub fn text(&self) -> String {
        match self.0.load(Ordering::Acquire) {
            0 => "Worker: idle / 后台待命".into(),
            u64::MAX => "Worker: 0/5".into(),
            n @ 1..=4 => format!("Worker: {n}/5"),
            n => format!("Worker: DONE 5/5 | actual elapsed: {} ms", n / 8),
        }
    }
}
pub const HINT: &str = "Drop files here / 拖入文件";
pub const START: &str = "5s worker test / 后台测试";
pub const CLEAR: &str = "Clear / 清空";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_finishes_without_ui_dispatch() {
        let w = Worker::default();
        w.start();
        assert!(w.running());
        let deadline = Instant::now() + Duration::from_secs(10);
        while w.running() && Instant::now() < deadline { thread::sleep(Duration::from_millis(50)); }
        assert!(w.text().contains("DONE 5/5"), "{}", w.text());
    }
}
