//! Telemetry example: collect encode phase timings.

use std::sync::{Arc, Mutex};
use tessellio_core::telemetry::{CodecEvent, EncodePhase, Telemetry};
use tessellio_core::{CapturedFrame, CodecConfig, ScreenStreamEncoder};

struct Log {
    entries: Mutex<Vec<(EncodePhase, std::time::Duration)>>,
}
impl Telemetry for Log {
    fn record(&self, event: CodecEvent) {
        if let CodecEvent::PhaseElapsed { phase, elapsed } = event {
            self.entries.lock().unwrap().push((phase, elapsed));
        }
    }
}

fn main() {
    let log = Arc::new(Log {
        entries: Mutex::new(Vec::new()),
    });
    let mut encoder = ScreenStreamEncoder::with_telemetry(CodecConfig::default(), log.clone());
    let rgba = vec![0u8; 320 * 180 * 4];
    let frame = CapturedFrame::new(320, 180, rgba).unwrap();
    let _ = encoder.encode(&frame);
    for (phase, dur) in log.entries.lock().unwrap().iter() {
        println!("{:?}: {:?}", phase, dur);
    }
}
