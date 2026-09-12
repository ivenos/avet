use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct HdrInfo {
    pub hdr_type: String,
    pub color_primaries: Option<u32>,
    pub transfer_characteristics: Option<u32>,
    pub matrix_coefficients: Option<u32>,
    pub chroma_sample_position: Option<u32>,
    /// Only set for full range. Studio is the encoder default and the common case.
    pub color_range: Option<u32>,
    pub content_light_level: Option<String>,
    pub mastering_display: Option<String>,
    /// Dolby Vision profile from the DOVI configuration record, when there is one.
    pub dv_profile: Option<u32>,
}

impl HdrInfo {
    pub fn is_hdr(&self) -> bool {
        !self.hdr_type.is_empty() && self.hdr_type != "SDR"
    }

    pub fn encoder_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if let Some(cp) = self.color_primaries {
            args.extend_from_slice(&["--color-primaries".into(), cp.to_string()]);
        }
        if let Some(tc) = self.transfer_characteristics {
            args.extend_from_slice(&["--transfer-characteristics".into(), tc.to_string()]);
        }
        if let Some(mc) = self.matrix_coefficients {
            args.extend_from_slice(&["--matrix-coefficients".into(), mc.to_string()]);
        }
        if let Some(csp) = self.chroma_sample_position {
            args.extend_from_slice(&["--chroma-sample-position".into(), csp.to_string()]);
        }
        if let Some(cr) = self.color_range {
            args.extend_from_slice(&["--color-range".into(), cr.to_string()]);
        }
        if let Some(ref cll) = self.content_light_level {
            args.extend_from_slice(&["--content-light".into(), cll.clone()]);
        }
        if let Some(ref mdl) = self.mastering_display {
            args.extend_from_slice(&["--mastering-display".into(), mdl.clone()]);
        }
        args
    }
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    frames: Vec<ProbeFrame>,
}

#[derive(Deserialize, Default)]
struct ProbeStream {
    #[serde(default)]
    color_primaries: String,
    #[serde(default)]
    color_transfer: String,
    #[serde(default)]
    color_space: String,
    #[serde(default)]
    chroma_location: String,
    #[serde(default)]
    color_range: String,
    /// Where the Dolby Vision configuration record lives, unlike the frame side data.
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

#[derive(Deserialize)]
struct ProbeFrame {
    #[serde(default)]
    side_data_list: Vec<SideData>,
}

#[derive(Deserialize)]
struct SideData {
    #[serde(default)]
    side_data_type: String,
    // Content Light Level
    max_content: Option<serde_json::Value>,
    max_average: Option<serde_json::Value>,
    // Mastering Display
    red_x: Option<serde_json::Value>,
    red_y: Option<serde_json::Value>,
    green_x: Option<serde_json::Value>,
    green_y: Option<serde_json::Value>,
    blue_x: Option<serde_json::Value>,
    blue_y: Option<serde_json::Value>,
    white_point_x: Option<serde_json::Value>,
    white_point_y: Option<serde_json::Value>,
    min_luminance: Option<serde_json::Value>,
    max_luminance: Option<serde_json::Value>,
    dv_profile: Option<serde_json::Value>,
}

pub fn detect(source_file: &Path) -> Result<HdrInfo> {
    let probe: ProbeOutput = crate::ext::ffprobe_json(
        &[
            "-v", "error",
            "-select_streams", "v:0",
            "-read_intervals", "%+#1",
            "-show_entries", "stream=color_primaries,color_transfer,color_space,chroma_location,color_range",
            // Sections accumulate, so this does not replace the two around it.
            "-show_entries", "stream_side_data=dv_profile",
            "-show_frames",
            "-show_entries", "frame=side_data_list",
            "-print_format", "json",
        ],
        source_file,
    )
    // Only called when the profile asks for HDR, so "no metadata" is not an answer here.
    .context("HDR detection")?;

    let stream = probe.streams.into_iter().next().unwrap_or_default();

    let mut info = HdrInfo {
        color_primaries: map_color_primaries(&stream.color_primaries),
        transfer_characteristics: map_transfer(&stream.color_transfer),
        matrix_coefficients: map_matrix(&stream.color_space),
        chroma_sample_position: map_chroma(&stream.chroma_location),
        color_range: map_color_range(&stream.color_range),
        ..Default::default()
    };

    let side_data = probe.frames.into_iter().next()
        .map(|f| f.side_data_list)
        .unwrap_or_default();

    let has_side_type = |needle: &str| {
        side_data.iter().any(|s| s.side_data_type.to_lowercase().contains(needle))
    };

    info.dv_profile = stream
        .side_data_list
        .iter()
        .find_map(|s| s.dv_profile.as_ref())
        .map(|v| val_to_i64(v) as u32);

    info.hdr_type = if info.dv_profile.is_some() || has_side_type("dolby") {
        "Dolby Vision".into()
    } else if has_side_type("hdr10+") {
        "HDR10+".into()
    } else if stream.color_transfer == "smpte2084" {
        "HDR10".into()
    } else if stream.color_transfer == "arib-std-b67" {
        "HLG".into()
    } else {
        "SDR".into()
    };

    for sd in &side_data {
        if info.content_light_level.is_none()
            && let (Some(mc), Some(ma)) = (&sd.max_content, &sd.max_average)
        {
            info.content_light_level =
                Some(format!("{},{}", val_to_i64(mc), val_to_i64(ma)));
        }
        if info.mastering_display.is_none()
            && let (Some(rx), Some(ry), Some(gx), Some(gy), Some(bx), Some(by),
                    Some(wpx), Some(wpy), Some(lmn), Some(lmx)) = (
                &sd.red_x, &sd.red_y, &sd.green_x, &sd.green_y,
                &sd.blue_x, &sd.blue_y, &sd.white_point_x, &sd.white_point_y,
                &sd.min_luminance, &sd.max_luminance,
            )
        {
            let (gx, gy)   = (val_to_f64(gx),  val_to_f64(gy));
            let (bx, by)   = (val_to_f64(bx),  val_to_f64(by));
            let (rx, ry)   = (val_to_f64(rx),  val_to_f64(ry));
            let (wpx, wpy) = (val_to_f64(wpx), val_to_f64(wpy));
            let (lmx, lmn) = (val_to_f64(lmx), val_to_f64(lmn));
            info.mastering_display = Some(format!(
                "G({gx:.4},{gy:.4})B({bx:.4},{by:.4})R({rx:.4},{ry:.4})\
                 WP({wpx:.4},{wpy:.4})L({lmx:.4},{lmn:.4})"
            ));
        }
    }

    // HLG has no static metadata by design; warn only for HDR10/HDR10+/DV.
    if info.is_hdr() && info.hdr_type != "HLG"
        && (info.content_light_level.is_none() || info.mastering_display.is_none())
    {
        let mut missing = Vec::new();
        if info.content_light_level.is_none() { missing.push("MaxCLL/MaxFALL"); }
        if info.mastering_display.is_none()   { missing.push("Mastering Display"); }
        tracing::warn!("HDR metadata incomplete - missing: {}", missing.join(", "));
    }

    Ok(info)
}

// ffprobe name to ITU-T H.273 numeric code (same values used by SVT-AV1)
fn map_color_primaries(s: &str) -> Option<u32> {
    Some(match s {
        "bt709"     => 1,
        "bt470m"    => 4,
        "bt470bg"   => 5,
        "smpte170m" => 6,
        "smpte240m" => 7,
        "film"      => 8,
        "bt2020"    => 9,
        "smpte428"  => 10,
        "smpte431"  => 11,
        "smpte432"  => 12,
        "ebu3213"   => 22,
        _           => return None,
    })
}

/// libavutil's names, not the AV1 spec's: `bt470m`, `bt470bg`, `log100`, `log316`, `bt1361e`.
fn map_transfer(s: &str) -> Option<u32> {
    Some(match s {
        "bt709"        => 1,
        "bt470m"       => 4,
        "bt470bg"      => 5,
        "smpte170m"    => 6,
        "smpte240m"    => 7,
        "linear"       => 8,
        "log100"       => 9,
        "log316"       => 10,
        "iec61966-2-4" => 11,
        "bt1361e"      => 12,
        "iec61966-2-1" => 13,
        "bt2020-10"    => 14,
        "bt2020-12"    => 15,
        "smpte2084"    => 16,
        "smpte428"     => 17,
        "arib-std-b67" => 18,
        _              => return None,
    })
}

fn map_matrix(s: &str) -> Option<u32> {
    Some(match s {
        "gbr"                => 0,
        "bt709"              => 1,
        "fcc"                => 4,
        "bt470bg"            => 5,
        "smpte170m"          => 6,
        "smpte240m"          => 7,
        "ycgco"              => 8,
        "bt2020nc"           => 9,
        "bt2020c"            => 10,
        "smpte2085"          => 11,
        "chroma-derived-nc"  => 12,
        "chroma-derived-c"   => 13,
        "ictcp"              => 14,
        _                    => return None,
    })
}

// SVT-AV1: 0=unknown, 1=vertical (left), 2=colocated (topleft); the rest has no AV1 form.
fn map_chroma(s: &str) -> Option<u32> {
    Some(match s {
        "left"    => 1,
        "topleft" => 2,
        _         => return None,
    })
}

// SVT-AV1: 0=studio, 1=full. Studio is the default, so only full needs signalling.
fn map_color_range(s: &str) -> Option<u32> {
    match s {
        "pc" | "full" => Some(1),
        _             => None,
    }
}

fn val_to_f64(v: &serde_json::Value) -> f64 {
    match v {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => {
            if let Some((num, den)) = s.split_once('/') {
                let n: f64 = num.trim().parse().unwrap_or(0.0);
                let d: f64 = den.trim().parse().unwrap_or(1.0);
                if d != 0.0 { n / d } else { 0.0 }
            } else {
                s.parse().unwrap_or(0.0)
            }
        }
        _ => 0.0,
    }
}

fn val_to_i64(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h273_color_primaries() {
        assert_eq!(map_color_primaries("bt709"),    Some(1));
        assert_eq!(map_color_primaries("bt2020"),   Some(9));
        assert_eq!(map_color_primaries("smpte432"), Some(12));
        assert_eq!(map_color_primaries("ebu3213"),  Some(22));
        assert_eq!(map_color_primaries("unknown"),  None);
        assert_eq!(map_color_primaries(""),         None);
    }

    #[test]
    fn h273_transfer() {
        assert_eq!(map_transfer("smpte2084"),    Some(16));
        assert_eq!(map_transfer("arib-std-b67"), Some(18));
        assert_eq!(map_transfer("bt2020-10"),    Some(14));
        assert_eq!(map_transfer("nope"),         None);
    }

    #[test]
    fn h273_matrix_uses_ffprobe_shortnames() {
        assert_eq!(map_matrix("bt2020nc"), Some(9));
        assert_eq!(map_matrix("bt2020c"),  Some(10));
        assert_eq!(map_matrix("ictcp"),    Some(14));
    }

    #[test]
    fn hdr_type_detection() {
        let mut i = HdrInfo::default();
        assert!(!i.is_hdr());
        i.hdr_type = "SDR".into();
        assert!(!i.is_hdr());
        i.hdr_type = "HDR10".into();
        assert!(i.is_hdr());
    }

    #[test]
    fn encoder_args_only_for_set_fields() {
        let i = HdrInfo {
            color_primaries: Some(9),
            transfer_characteristics: Some(16),
            ..Default::default()
        };
        let args = i.encoder_args();
        assert_eq!(args, vec![
            "--color-primaries", "9",
            "--transfer-characteristics", "16",
        ]);
    }

    #[test]
    fn transfer_uses_the_names_ffprobe_actually_prints() {
        // Verified against ffprobe 8.1; these five differ from the AV1 spec spelling.
        assert_eq!(map_transfer("bt470m"),  Some(4));
        assert_eq!(map_transfer("bt470bg"), Some(5));
        assert_eq!(map_transfer("log100"),  Some(9));
        assert_eq!(map_transfer("log316"),  Some(10));
        assert_eq!(map_transfer("bt1361e"), Some(12));

        assert_eq!(map_transfer("gamma22"), None);
        assert_eq!(map_transfer("log"),     None);
        assert_eq!(map_transfer("unknown"), None);
    }

    #[test]
    fn dv_profile_is_read_from_stream_side_data() {
        // The frame side data carries the RPU but no profile number.
        let json = r#"{
            "streams": [{
                "color_transfer": "smpte2084",
                "side_data_list": [{"side_data_type": "DOVI configuration record",
                                    "dv_profile": 5}]
            }],
            "frames": [{"side_data_list": [{"side_data_type": "Dolby Vision RPU Data"}]}]
        }"#;
        let probe: ProbeOutput = serde_json::from_str(json).unwrap();
        let stream = probe.streams.into_iter().next().unwrap();
        let profile = stream.side_data_list.iter()
            .find_map(|s| s.dv_profile.as_ref())
            .map(|v| val_to_i64(v) as u32);
        assert_eq!(profile, Some(5));
    }

    #[test]
    fn val_to_f64_parses_rational() {
        let v = serde_json::Value::String("50000/10000".into());
        assert_eq!(val_to_f64(&v), 5.0);
        let v = serde_json::Value::String("not-a-number".into());
        assert_eq!(val_to_f64(&v), 0.0);
    }
}
