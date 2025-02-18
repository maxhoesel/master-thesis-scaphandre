use crate::sensors::utils::current_system_time_since_epoch;
use crate::sensors::Record;
use std::io::ErrorKind;
use std::process::Command;

use super::units::Unit;

pub struct IpmptoolSensor {}

impl IpmptoolSensor {
    /// Instantiates and returns an instance of PowercapRAPLSensor.
    pub fn new() -> IpmptoolSensor {
        IpmptoolSensor {}
    }

    pub fn read_power(&self) -> Option<Record> {
        let cmd = match Command::new("ipmitool")
            .args(["dcmi", "power", "reading"])
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                if e.kind() != ErrorKind::NotFound {
                    warn!("Unable to get ipmi reading: {e}");
                }
                return None;
            }
        };
        let stdout = String::from_utf8_lossy(&cmd.stdout).to_string();

        let power_reading: u32 = stdout
            .lines()
            .find_map(|l| {
                let trim = l.trim();
                if trim.starts_with("Instantaneous power reading") {
                    Some(trim)
                } else {
                    None
                }
            })
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()[3]
            .parse()
            .unwrap();
        Some(Record::new(
            current_system_time_since_epoch(),
            power_reading.to_string(),
            Unit::Watt,
        ))
    }
}

impl Default for IpmptoolSensor {
    fn default() -> Self {
        Self::new()
    }
}
