//! Settings in /etc/circular-scroll.toml.

use std::{fs, io, path::Path};

pub const DEFAULT_PATH: &str = "/etc/circular-scroll.toml";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Exact device name; empty = auto-detect the first touchpad.
    pub device_name: String,
    /// Pad geometry in raw touchpad units. 0 = derive from the device's reported range.
    pub center_x: f64,
    pub center_y: f64,
    pub radius_x: f64,
    pub radius_y: f64,
    /// A touch starting at or beyond this fraction of the radius enters scroll mode.
    pub ring_start: f64,
    /// Degrees of rotation per wheel notch (smaller = faster scrolling).
    pub degrees_per_notch: f64,
    /// Flip direction (true: clockwise scrolls up).
    pub reverse: bool,
    /// Ignore angle updates when the finger drifts this close to the centre.
    pub min_track_radius: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            device_name: String::new(),
            center_x: 0.0,
            center_y: 0.0,
            radius_x: 0.0,
            radius_y: 0.0,
            ring_start: 0.70,
            degrees_per_notch: 20.0,
            reverse: false,
            min_track_radius: 0.35,
        }
    }
}

const KEYS: [&str; 9] = [
    "device_name",
    "center_x",
    "center_y",
    "radius_x",
    "radius_y",
    "ring_start",
    "degrees_per_notch",
    "reverse",
    "min_track_radius",
];

fn comment(key: &str) -> Option<&'static str> {
    Some(match key {
        "device_name" => "exact input device name; empty = auto-detect touchpad",
        "center_x" => "pad geometry in raw touchpad units (set by calibration)",
        "ring_start" => "touches starting beyond this fraction of the radius scroll",
        "degrees_per_notch" => "smaller = faster scrolling",
        "reverse" => "true: clockwise scrolls up",
        "min_track_radius" => "ignore movement this close to the centre while scrolling",
        _ => return None,
    })
}

impl Config {
    /// Load settings, falling back to defaults when the file doesn't exist.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Config::parse(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| e.message().to_string())?;
        let mut cfg = Config::default();
        let mut unknown = Vec::new();
        for (key, value) in &table {
            let num = || match value {
                toml::Value::Integer(i) => Ok(*i as f64),
                toml::Value::Float(f) => Ok(*f),
                _ => Err(format!("{key} must be a number")),
            };
            match key.as_str() {
                "device_name" => {
                    cfg.device_name = value
                        .as_str()
                        .ok_or("device_name must be a string")?
                        .to_string()
                }
                "center_x" => cfg.center_x = num()?,
                "center_y" => cfg.center_y = num()?,
                "radius_x" => cfg.radius_x = num()?,
                "radius_y" => cfg.radius_y = num()?,
                "ring_start" => cfg.ring_start = num()?,
                "degrees_per_notch" => cfg.degrees_per_notch = num()?,
                "min_track_radius" => cfg.min_track_radius = num()?,
                "reverse" => {
                    cfg.reverse = value.as_bool().ok_or("reverse must be true or false")?
                }
                _ => unknown.push(key.clone()),
            }
        }
        if !unknown.is_empty() {
            eprintln!("warning: unknown config keys ignored: {unknown:?}");
        }
        if cfg.degrees_per_notch <= 0.0 {
            return Err("degrees_per_notch must be greater than 0".into());
        }
        Ok(cfg)
    }

    fn value(&self, key: &str) -> String {
        match key {
            "device_name" => toml_string(&self.device_name),
            "center_x" => int(self.center_x),
            "center_y" => int(self.center_y),
            "radius_x" => int(self.radius_x),
            "radius_y" => int(self.radius_y),
            "ring_start" => float(self.ring_start),
            "degrees_per_notch" => float(self.degrees_per_notch),
            "reverse" => self.reverse.to_string(),
            "min_track_radius" => float(self.min_track_radius),
            _ => unreachable!(),
        }
    }

    /// One `key = value` line, as printed after calibration.
    pub fn line(&self, key: &str) -> String {
        format!("{key} = {}", self.value(key))
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# circular-scroll settings\n# apply changes with: sudo systemctl restart circular-scroll\n\n",
        );
        for key in KEYS {
            let line = self.line(key);
            match comment(key) {
                Some(c) => out.push_str(&format!("{line:<40} # {c}\n")),
                None => out.push_str(&format!("{line}\n")),
            }
        }
        out
    }

    /// Write atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        fs::write(&tmp, self.to_toml())?;
        fs::rename(&tmp, path)
    }
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn int(v: f64) -> String {
    format!("{}", v.round() as i64)
}

/// Floats always keep a decimal point so TOML reads them back as floats.
fn float(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let cfg = Config {
            device_name: "SynPS/2 \"Synaptics\" TouchPad".into(),
            center_x: 3571.0,
            center_y: 2854.0,
            radius_x: 2553.0,
            radius_y: 1962.0,
            ring_start: 0.7,
            degrees_per_notch: 20.0,
            reverse: true,
            min_track_radius: 0.35,
        };
        assert_eq!(Config::parse(&cfg.to_toml()).unwrap(), cfg);
    }

    #[test]
    fn reads_the_file_the_python_version_wrote() {
        let text = r#"# circular-scroll settings
device_name = "SynPS/2 Synaptics TouchPad" # exact input device name
center_x = 3571
center_y = 2854
radius_x = 2553
radius_y = 1962
ring_start = 0.7
degrees_per_notch = 20
reverse = false
min_track_radius = 0.35
"#;
        let cfg = Config::parse(text).unwrap();
        assert_eq!(cfg.center_x, 3571.0);
        assert_eq!(cfg.degrees_per_notch, 20.0);
        assert_eq!(cfg.device_name, "SynPS/2 Synaptics TouchPad");
    }

    #[test]
    fn missing_keys_use_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn rejects_bad_types() {
        assert!(Config::parse("reverse = 1").is_err());
        assert!(Config::parse("ring_start = \"wide\"").is_err());
    }

    #[test]
    fn writes_python_compatible_numbers() {
        let text = Config::default().to_toml();
        assert!(text.contains("degrees_per_notch = 20.0"));
        assert!(text.contains("ring_start = 0.7 "));
        assert!(text.contains("center_x = 0 "));
    }
}
