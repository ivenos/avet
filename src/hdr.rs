use anyhow::{bail, Context, Result};
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use dolby_vision::rpu::extension_metadata::blocks::ExtMetadataBlock;
use dolby_vision::rpu::ConversionMode;
use serde::Deserialize;
use std::path::Path;

use crate::ffms2::{Crop, FrameHdrMetadata};

#[derive(Debug, Default, Clone)]
pub struct HdrInfo {
    pub codec_name: String,
    pub hdr_type: String,
    pub color_primaries: Option<u32>,
    pub transfer_characteristics: Option<u32>,
    pub matrix_coefficients: Option<u32>,
    pub chroma_sample_position: Option<u32>,
    /// JPEG's siting, which AV1 cannot signal but Matroska can.
    pub chroma_center: bool,
    /// Only set for full range. Studio is the encoder default and the common case.
    pub color_range: Option<u32>,
    pub content_light_level: Option<String>,
    pub mastering_display: Option<String>,
    /// Dolby Vision profile from the DOVI configuration record, when there is one.
    pub dv_profile: Option<u32>,
    /// HDR10+ on the first frame; also set under Dolby Vision, which wins `hdr_type`.
    pub hdr10plus: bool,
}

impl HdrInfo {
    pub fn is_hdr(&self) -> bool {
        !self.hdr_type.is_empty() && self.hdr_type != "SDR"
    }

    pub fn encoder_args(&self) -> Vec<String> {
        let mut args = self.colour_args();
        if let Some(ref cll) = self.content_light_level {
            args.extend_from_slice(&["--content-light".into(), cll.clone()]);
        }
        if let Some(ref mdl) = self.mastering_display {
            args.extend_from_slice(&["--mastering-display".into(), mdl.clone()]);
        }
        args
    }

    fn colour_args(&self) -> Vec<String> {
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
        args
    }

    /// HLG has no static metadata by design.
    pub fn missing_static_metadata(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.is_hdr() && self.hdr_type != "HLG" {
            if self.content_light_level.is_none() { missing.push("MaxCLL/MaxFALL"); }
            if self.mastering_display.is_none()   { missing.push("Mastering Display"); }
        }
        missing
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
    codec_name: String,
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
            "-show_entries", "stream=codec_name,color_primaries,color_transfer,color_space,chroma_location,color_range",
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
        codec_name: stream.codec_name.clone(),
        color_primaries: map_color_primaries(&stream.color_primaries),
        transfer_characteristics: map_transfer(&stream.color_transfer),
        matrix_coefficients: map_matrix(&stream.color_space),
        chroma_sample_position: map_chroma(&stream.chroma_location),
        chroma_center: stream.chroma_location == "center",
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

    info.hdr10plus = has_side_type("hdr10+");
    info.hdr_type = if info.dv_profile.is_some() || has_side_type("dolby") {
        "Dolby Vision".into()
    } else if info.hdr10plus {
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
            // Unrounded: the source's 1/50000 steps fall between 4-decimal ones.
            info.mastering_display = Some(format!(
                "G({gx},{gy})B({bx},{by})R({rx},{ry})WP({wpx},{wpy})L({lmx},{lmn})"
            ));
        }
    }

    Ok(info)
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct DynamicHdr {
    pub hdr10plus: bool,
    pub dolby_vision: bool,
}

impl DynamicHdr {
    pub fn any(self) -> bool {
        self.hdr10plus || self.dolby_vision
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub crop: Option<Crop>,
    pub scale: Option<(u32, u32)>,
}

const HDR10PLUS_T35_HEADER: [u8; 6] = [0xB5, 0x00, 0x3C, 0x00, 0x01, 0x04];

/// ITU-T T.35 messages for one frame, country code first, in the order SVT-AV1 writes them.
pub fn t35_messages(frame: &FrameHdrMetadata, carry: DynamicHdr, geometry: Geometry) -> Result<Vec<Vec<u8>>> {
    let mut messages = Vec::new();
    if carry.dolby_vision
        && let Some(rpu) = &frame.dovi_rpu
    {
        messages.push(dovi_t35(rpu, geometry)?);
    }
    if carry.hdr10plus
        && let Some(st2094_40) = &frame.hdr10plus
    {
        messages.push([&HDR10PLUS_T35_HEADER[..], st2094_40].concat());
    }
    Ok(messages)
}

fn dovi_t35(nal_payload: &[u8], geometry: Geometry) -> Result<Vec<u8>> {
    let mut rpu = DoviRpu::parse_unspec62_nalu(nal_payload).context("parse Dolby Vision RPU")?;
    match rpu.dovi_profile {
        5 | 8 => {}
        7 => rpu.convert_with_mode(ConversionMode::To81).context("convert Dolby Vision profile 7 to 8.1")?,
        p => bail!("Dolby Vision profile {p} has no AV1 form"),
    }
    let active_area = rpu.vdr_dm_data.as_ref().and_then(|dm| match dm.get_block(5) {
        Some(ExtMetadataBlock::Level5(l5)) => Some((
            l5.active_area_left_offset, l5.active_area_right_offset,
            l5.active_area_top_offset, l5.active_area_bottom_offset,
        )),
        _ => None,
    });
    if let Some(offsets) = active_area {
        let (left, right, top, bottom) = remap_active_area(offsets, geometry);
        if (left, right, top, bottom) != offsets {
            rpu.set_active_area_offsets(left, right, top, bottom)?;
        }
    }
    rpu.write_av1_rpu_metadata_obu_t35_complete().context("write Dolby Vision RPU for AV1")
}

fn remap_active_area((left, right, top, bottom): (u16, u16, u16, u16), g: Geometry) -> (u16, u16, u16, u16) {
    let c = g.crop.unwrap_or(Crop { w: g.width, h: g.height, x: 0, y: 0 });
    let (out_w, out_h) = g.scale.unwrap_or((c.w, c.h));
    let fit = |offset: u16, cut: u32, kept: u32, out: u32| {
        let px = u64::from(u32::from(offset).saturating_sub(cut).min(kept));
        let kept = u64::from(kept.max(1));
        ((px * u64::from(out) + kept / 2) / kept) as u16
    };
    (
        fit(left, c.x, c.w, out_w),
        fit(right, g.width.saturating_sub(c.x + c.w), c.w, out_w),
        fit(top, c.y, c.h, out_h),
        fit(bottom, g.height.saturating_sub(c.y + c.h), c.h, out_h),
    )
}

/// The colour flags among `encoder_args` as mkvmerge options for `track`. mkvmerge takes
/// none of them from an IVF input.
pub fn mkvmerge_colour_args(encoder_args: &[String], track: u32) -> Vec<String> {
    let value = |flag: &str| {
        encoder_args.chunks(2).find(|p| p.len() == 2 && p[0] == flag).map(|p| p[1].as_str())
    };
    let number = |flag: &str| value(flag).and_then(|v| v.parse::<u32>().ok());

    let mut args = Vec::new();
    let mut push = |flag: &str, v: String| {
        args.push(flag.to_string());
        args.push(format!("{track}:{v}"));
    };
    let described = ["--color-primaries", "--transfer-characteristics", "--matrix-coefficients"]
        .iter()
        .any(|f| number(f).is_some());

    if let Some(v) = number("--matrix-coefficients") {
        push("--color-matrix-coefficients", v.to_string());
    }
    if let Some(v) = number("--transfer-characteristics") {
        push("--color-transfer-characteristics", v.to_string());
    }
    if let Some(v) = number("--color-primaries") {
        push("--color-primaries", v.to_string());
    }
    if described {
        // SVT-AV1: 0 studio, 1 full. Matroska: 1 broadcast, 2 full.
        let range = if number("--color-range") == Some(1) { 2 } else { 1 };
        push("--color-range", range.to_string());
    }
    // Horizontal, then vertical: 1 is co-sited, 2 is half a sample off.
    match number("--chroma-sample-position") {
        Some(1) => push("--chroma-siting", "1,2".into()),
        Some(2) => push("--chroma-siting", "1,1".into()),
        _ => {}
    }
    if let Some((max_cll, max_fall)) = value("--content-light").and_then(|v| v.split_once(',')) {
        push("--max-content-light", max_cll.trim().to_string());
        push("--max-frame-light", max_fall.trim().to_string());
    }
    if let Some(md) = value("--mastering-display").and_then(parse_mastering_display) {
        let [g, b, r, wp, l] = md;
        push("--chromaticity-coordinates", format!("{},{},{},{},{},{}", r.0, r.1, g.0, g.1, b.0, b.1));
        push("--white-color-coordinates", format!("{},{}", wp.0, wp.1));
        push("--max-luminance", l.0.to_string());
        push("--min-luminance", l.1.to_string());
    }
    args
}

fn parse_mastering_display(s: &str) -> Option<[(f64, f64); 5]> {
    let pair = |label: &str| -> Option<(f64, f64)> {
        let rest = &s[s.find(label)? + label.len()..];
        let (a, b) = rest[..rest.find(')')?].split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    Some([pair("G(")?, pair("B(")?, pair("R(")?, pair("WP(")?, pair("L(")?])
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

    fn geometry(crop: Option<Crop>, scale: Option<(u32, u32)>) -> Geometry {
        Geometry { width: 1920, height: 1080, crop, scale }
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn active_area_never_leaves_the_picture(
            (w, h, x, y, cw, ch) in (2u32..4096, 2u32..4096).prop_flat_map(|(w, h)| {
                (Just(w), Just(h), 0..w, 0..h).prop_flat_map(|(w, h, x, y)| (Just(w), Just(h), Just(x), Just(y), 1..=w - x, 1..=h - y))
            }),
            scale in prop::option::of(0.05f64..1.0),
            offsets in (0u16..8192, 0u16..8192, 0u16..8192, 0u16..8192),
        ) {
            let crop = Crop { w: cw, h: ch, x, y };
            let out = scale.map(|f| (((cw as f64 * f) as u32).max(1), ((ch as f64 * f) as u32).max(1)));
            let g = Geometry { width: w, height: h, crop: Some(crop), scale: out };
            let (l, r, t, b) = remap_active_area(offsets, g);
            let (ow, oh) = out.unwrap_or((cw, ch));
            prop_assert!(u32::from(l) <= ow && u32::from(r) <= ow && u32::from(t) <= oh && u32::from(b) <= oh);

            let untouched = Geometry { width: w, height: h, crop: None, scale: None };
            let clamp = |v: u16, max: u32| v.min(max as u16);
            prop_assert_eq!(remap_active_area(offsets, untouched), (clamp(offsets.0, w), clamp(offsets.1, w), clamp(offsets.2, h), clamp(offsets.3, h)));
        }

        #[test]
        fn mastering_display_values_survive_the_round_trip(
            v in prop::collection::vec(0.0f64..1.0, 8),
            max in 1.0f64..10000.0,
            min in 0.0f64..1.0,
        ) {
            let s = format!("G({},{})B({},{})R({},{})WP({},{})L({max},{min})", v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
            let parsed = parse_mastering_display(&s).unwrap();
            prop_assert_eq!(parsed, [(v[0], v[1]), (v[2], v[3]), (v[4], v[5]), (v[6], v[7]), (max, min)]);
        }
    }

    #[test]
    fn active_area_follows_crop_and_scale() {
        let bars = (0, 0, 140, 140);
        assert_eq!(remap_active_area(bars, geometry(None, None)), bars);

        let exact = Crop { w: 1920, h: 800, x: 0, y: 140 };
        assert_eq!(remap_active_area(bars, geometry(Some(exact), None)), (0, 0, 0, 0));

        let partial = Crop { w: 1920, h: 1000, x: 0, y: 40 };
        assert_eq!(remap_active_area(bars, geometry(Some(partial), None)), (0, 0, 100, 100));

        assert_eq!(remap_active_area(bars, geometry(None, Some((1280, 720)))), (0, 0, 93, 93));
        assert_eq!(remap_active_area(bars, geometry(Some(partial), Some((960, 500)))), (0, 0, 50, 50));
    }

    fn ffms2_rpu(offsets: (u16, u16, u16, u16)) -> Vec<u8> {
        use dolby_vision::rpu::generate::GenerateConfig;
        let mut rpu = DoviRpu::profile81_config(&GenerateConfig { length: 1, ..Default::default() }).unwrap();
        rpu.set_active_area_offsets(offsets.0, offsets.1, offsets.2, offsets.3).unwrap();
        // FFMS2 drops the two NAL header bytes and keeps emulation prevention.
        rpu.write_hevc_unspec62_nalu().unwrap()[2..].to_vec()
    }

    fn l5(t35: &[u8]) -> (u16, u16, u16, u16) {
        let rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35).unwrap();
        match rpu.vdr_dm_data.unwrap().get_block(5) {
            Some(ExtMetadataBlock::Level5(b)) => (
                b.active_area_left_offset, b.active_area_right_offset,
                b.active_area_top_offset, b.active_area_bottom_offset,
            ),
            _ => panic!("no L5 block"),
        }
    }

    #[test]
    fn t35_messages_wrap_both_kinds_and_skip_what_is_not_carried() {
        let frame = FrameHdrMetadata {
            dovi_rpu: Some(ffms2_rpu((0, 0, 140, 140))),
            hdr10plus: Some(vec![0x01, 0x40, 0x00]),
        };
        let both = DynamicHdr { hdr10plus: true, dolby_vision: true };
        let crop = Crop { w: 1920, h: 800, x: 0, y: 140 };

        let messages = t35_messages(&frame, both, geometry(Some(crop), None)).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0][..3], [0xB5, 0x00, 0x3B]);
        assert_eq!(l5(&messages[0]), (0, 0, 0, 0));
        assert_eq!(messages[1], [0xB5, 0x00, 0x3C, 0x00, 0x01, 0x04, 0x01, 0x40, 0x00]);

        let uncropped = t35_messages(&frame, both, geometry(None, None)).unwrap();
        assert_eq!(l5(&uncropped[0]), (0, 0, 140, 140));

        let hdr10plus_only = DynamicHdr { hdr10plus: true, dolby_vision: false };
        let messages = t35_messages(&frame, hdr10plus_only, geometry(None, None)).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0][2], 0x3C);
    }

    #[test]
    fn colour_flags_become_mkvmerge_options() {
        let args: Vec<String> = [
            "--crf", "30",
            "--color-primaries", "9", "--transfer-characteristics", "16",
            "--matrix-coefficients", "9", "--chroma-sample-position", "2",
            "--content-light", "1000,400",
            "--mastering-display", "G(0.2650,0.6900)B(0.1500,0.0600)R(0.6800,0.3200)WP(0.3127,0.3290)L(1000.0000,0.0050)",
        ].iter().map(|s| s.to_string()).collect();

        assert_eq!(mkvmerge_colour_args(&args, 0), [
            "--color-matrix-coefficients", "0:9",
            "--color-transfer-characteristics", "0:16",
            "--color-primaries", "0:9",
            "--color-range", "0:1",
            "--chroma-siting", "0:1,1",
            "--max-content-light", "0:1000",
            "--max-frame-light", "0:400",
            "--chromaticity-coordinates", "0:0.68,0.32,0.265,0.69,0.15,0.06",
            "--white-color-coordinates", "0:0.3127,0.329",
            "--max-luminance", "0:1000",
            "--min-luminance", "0:0.005",
        ]);

        let full = ["--color-primaries", "1", "--color-range", "1"].map(String::from);
        assert!(mkvmerge_colour_args(&full, 0).windows(2).any(|w| w == ["--color-range", "0:2"]));
        assert!(mkvmerge_colour_args(&["--crf".to_string(), "30".to_string()], 0).is_empty());
    }

    #[test]
    fn val_to_f64_parses_rational() {
        let v = serde_json::Value::String("50000/10000".into());
        assert_eq!(val_to_f64(&v), 5.0);
        let v = serde_json::Value::String("not-a-number".into());
        assert_eq!(val_to_f64(&v), 0.0);
    }
}
