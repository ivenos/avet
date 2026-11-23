#![allow(non_camel_case_types, non_snake_case)]

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::io::Write;
use std::os::raw::{c_char, c_double, c_int, c_uint};
use std::path::Path;
use std::sync::{Once, OnceLock};

use crate::ext::external_bin;

const FFMS_ERROR_BUFFER_SIZE: usize = 1024;
const FFMS_SEEK_LINEAR: c_int = 0;
const FFMS_SEEK_NORMAL: c_int = 1;
const FFMS_TYPE_VIDEO: c_int = 0;
const FFMS_RESIZER_BICUBIC: c_int  = 4;

// Must match FFMS2's FFMS_ErrorInfo exactly: ErrorType, SubType, BufferSize, Buffer
#[repr(C)]
struct RawErrorInfo {
    error_type: c_int,
    sub_type: c_int,
    buffer_size: c_int,
    buffer: *mut c_char,
}

#[repr(C)]
pub struct FFMS_VideoSource {
    _private: [u8; 0],
}

#[repr(C)]
pub struct FFMS_Index {
    _private: [u8; 0],
}

#[repr(C)]
pub struct FFMS_Track {
    _private: [u8; 0],
}

#[repr(C)]
pub struct FFMS_TrackTimeBase {
    pub num: i64,
    pub den: i64,
}

#[repr(C)]
pub struct FFMS_FrameInfo {
    pub pts: i64,
    pub repeat_pict: c_int,
    pub key_frame: c_int,
    pub original_pts: i64,
}

#[repr(C)]
pub struct FFMS_Frame {
    pub data: [*const u8; 4],
    pub linesize: [c_int; 4],
    pub encoded_width: c_int,
    pub encoded_height: c_int,
    pub encoded_pixel_format: c_int,
    pub scaled_width: c_int,
    pub scaled_height: c_int,
    pub converted_pixel_format: c_int,
    pub key_frame: c_int,
    pub repeat_pict: c_int,
    pub interlaced_frame: c_int,
    pub top_field_first: c_int,
    pub pict_type: c_char,
    pub color_space: c_int,
    pub color_range: c_int,
    pub color_primaries: c_int,
    pub transfer_characteristics: c_int,
    pub chroma_location: c_int,
    pub has_mastering_display_primaries: c_int,
    pub mastering_display_primaries_x: [c_double; 3],
    pub mastering_display_primaries_y: [c_double; 3],
    pub mastering_display_white_point_x: c_double,
    pub mastering_display_white_point_y: c_double,
    pub has_mastering_display_luminance: c_int,
    pub mastering_display_min_luminance: c_double,
    pub mastering_display_max_luminance: c_double,
    pub has_content_light_level: c_int,
    pub content_light_level_max: c_uint,
    pub content_light_level_average: c_uint,
    pub dolby_vision_rpu: *const u8,
    pub dolby_vision_rpu_size: c_int,
    pub hdr10_plus: *const u8,
    pub hdr10_plus_size: c_int,
}

#[repr(C)]
pub struct FFMS_VideoProperties {
    // Denominator first, in both pairs. ffms.h really is ordered that way.
    pub fps_denominator: c_int,
    pub fps_numerator: c_int,
    pub rff_denominator: c_int,
    pub rff_numerator: c_int,
    pub num_frames: c_int,
    pub sar_num: c_int,
    pub sar_den: c_int,
    pub crop_top: c_int,
    pub crop_bottom: c_int,
    pub crop_left: c_int,
    pub crop_right: c_int,
    pub top_field_first: c_int,
    pub color_space: c_int,
    pub color_range: c_int,
    pub first_time: c_double,
    pub last_time: c_double,
    pub rotation: c_int,
    pub stereo3d_type: c_int,
    pub stereo3d_flags: c_int,
    pub last_end_time: c_double,
    pub has_mastering_display_primaries: c_int,
    pub mastering_display_primaries_x: [c_double; 3],
    pub mastering_display_primaries_y: [c_double; 3],
    pub mastering_display_white_point_x: c_double,
    pub mastering_display_white_point_y: c_double,
    pub has_mastering_display_luminance: c_int,
    pub mastering_display_min_luminance: c_double,
    pub mastering_display_max_luminance: c_double,
    pub has_content_light_level: c_int,
    pub content_light_level_max: c_uint,
    pub content_light_level_average: c_uint,
    pub flip: c_int,
}

#[link(name = "ffms2")]
unsafe extern "C" {
    fn FFMS_Init(reserved: c_int, deprecated: c_int);
    fn FFMS_GetPixFmt(name: *const c_char) -> c_int;
    fn FFMS_ReadIndex(index_file: *const c_char, error_info: *mut RawErrorInfo) -> *mut FFMS_Index;
    fn FFMS_DestroyIndex(index: *mut FFMS_Index);
    fn FFMS_GetFirstTrackOfType(
        index: *mut FFMS_Index,
        track_type: c_int,
        error_info: *mut RawErrorInfo,
    ) -> c_int;
    fn FFMS_CreateVideoSource(
        source_file: *const c_char,
        track: c_int,
        index: *mut FFMS_Index,
        threads: c_int,
        seek_mode: c_int,
        error_info: *mut RawErrorInfo,
    ) -> *mut FFMS_VideoSource;
    fn FFMS_DestroyVideoSource(v: *mut FFMS_VideoSource);
    fn FFMS_GetVideoProperties(v: *mut FFMS_VideoSource) -> *const FFMS_VideoProperties;
    fn FFMS_SetOutputFormatV2(
        v: *mut FFMS_VideoSource,
        target_formats: *const c_int,
        width: c_int,
        height: c_int,
        resizer: c_int,
        error_info: *mut RawErrorInfo,
    ) -> c_int;
    fn FFMS_GetFrame(
        v: *mut FFMS_VideoSource,
        n: c_int,
        error_info: *mut RawErrorInfo,
    ) -> *const FFMS_Frame;
    fn FFMS_GetTrackFromVideo(v: *mut FFMS_VideoSource) -> *mut FFMS_Track;
    fn FFMS_GetNumFrames(t: *mut FFMS_Track) -> c_int;
    fn FFMS_GetFrameInfo(t: *mut FFMS_Track, frame: c_int) -> *const FFMS_FrameInfo;
    fn FFMS_GetTimeBase(t: *mut FFMS_Track) -> *const FFMS_TrackTimeBase;
}

struct ErrorInfo {
    buf: Box<[c_char; FFMS_ERROR_BUFFER_SIZE]>,
    pub raw: RawErrorInfo,
}

impl ErrorInfo {
    fn new() -> Self {
        let mut buf = Box::new([0; FFMS_ERROR_BUFFER_SIZE]);
        let raw = RawErrorInfo {
            error_type: 0,
            sub_type: 0,
            buffer_size: FFMS_ERROR_BUFFER_SIZE as c_int,
            buffer: buf.as_mut_ptr(),
        };
        ErrorInfo { buf, raw }
    }

    fn message(&self) -> String {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, FFMS_ERROR_BUFFER_SIZE)
        };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).to_string()
    }
}

#[derive(Debug, Default, Clone)]
pub struct FrameHdrMetadata {
    pub dovi_rpu: Option<Vec<u8>>,
    pub hdr10plus: Option<Vec<u8>>,
}

unsafe fn side_data(ptr: *const u8, size: c_int) -> Option<Vec<u8>> {
    let len = usize::try_from(size).ok().filter(|&n| n > 0)?;
    (!ptr.is_null()).then(|| unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

pub fn get_pixel_format(name: &str) -> c_int {
    let Ok(cname) = CString::new(name) else { return -1 };
    unsafe { FFMS_GetPixFmt(cname.as_ptr()) }
}

/// crop=W:H:X:Y, applied in the Y4M pipe so the encoder only sees the cropped frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub w: u32,
    pub h: u32,
    pub x: u32,
    pub y: u32,
}

impl Crop {
    pub fn from_str(s: &str) -> Option<Self> {
        let s = s.trim_start_matches("crop=");
        let p: Vec<&str> = s.split(':').collect();
        if p.len() != 4 {
            return None;
        }
        Some(Crop {
            w: p[0].parse().ok()?,
            h: p[1].parse().ok()?,
            x: p[2].parse().ok()?,
            y: p[3].parse().ok()?,
        })
    }

    /// Even edges, inside the source. The single check; Y4M pointer arithmetic,
    /// half-sized chroma and FFVship's edge offsets all rely on it.
    pub fn normalized(self, src_w: u32, src_h: u32) -> Option<Self> {
        let c = Crop { w: self.w & !1, h: self.h & !1, x: self.x & !1, y: self.y & !1 };
        if c.w == 0 || c.h == 0 {
            return None;
        }
        if c.x.checked_add(c.w)? > src_w || c.y.checked_add(c.h)? > src_h {
            return None;
        }
        Some(c)
    }

    pub fn to_filter(self) -> String {
        format!("crop={}:{}:{}:{}", self.w, self.h, self.x, self.y)
    }

    /// Smallest rectangle containing both, so no sample's content is cut away.
    pub fn union(self, other: Self) -> Self {
        // Runs on raw cropdetect output, before `normalized` rejects anything.
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right  = self.x.saturating_add(self.w).max(other.x.saturating_add(other.w));
        let bottom = self.y.saturating_add(self.h).max(other.y.saturating_add(other.h));
        Crop { w: right - x, h: bottom - y, x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    Horizontal,
    Vertical,
}

impl Mirror {
    pub fn filter(self) -> &'static str {
        match self {
            Mirror::Horizontal => "hflip",
            Mirror::Vertical => "vflip",
        }
    }

    /// Chroma sited at the left or top edge of its luma pair lands at the far edge once flipped.
    pub fn moves_chroma(self, loc: &str, subsampling: PixelSubsampling) -> bool {
        match self {
            Mirror::Horizontal => subsampling != PixelSubsampling::Yuv444 && loc.ends_with("left"),
            Mirror::Vertical => subsampling == PixelSubsampling::Yuv420 && (loc.starts_with("top") || loc.starts_with("bottom")),
        }
    }
}

/// How a player shows the stored picture: mirrored first, then turned counter-clockwise by `roll` degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Orientation {
    pub mirror: Option<Mirror>,
    pub roll: i64,
}

impl Orientation {
    /// FFMS2 applies `flip` before its clockwise `rotation`.
    pub fn from_ffms2(flip: c_int, rotation: c_int) -> Self {
        let clockwise = i64::from(rotation).rem_euclid(360);
        // FFMS2 reports a vertical mirror as a horizontal one turned by 180 degrees.
        let (mirror, clockwise) = match flip.signum() {
            0 => (None, clockwise),
            1 if clockwise == 180 => (Some(Mirror::Vertical), 0),
            1 => (Some(Mirror::Horizontal), clockwise),
            _ if clockwise == 180 => (Some(Mirror::Horizontal), 0),
            _ => (Some(Mirror::Vertical), clockwise),
        };
        let roll = (360 - clockwise) % 360;
        Orientation { mirror, roll: if roll > 180 { roll - 360 } else { roll } }
    }
}

#[cfg(test)]
mod crop_tests {
    use super::Crop;

    fn c(w: u32, h: u32, x: u32, y: u32) -> Crop {
        Crop { w, h, x, y }
    }

    #[test]
    fn parses_both_spellings_and_rejects_short_lists() {
        assert_eq!(Crop::from_str("crop=640:360:0:60"), Some(c(640, 360, 0, 60)));
        assert_eq!(Crop::from_str("640:360:0:60"), Some(c(640, 360, 0, 60)));
        assert_eq!(Crop::from_str("640:360"), None);
        assert_eq!(Crop::from_str("640:360:0:60:8"), None);
    }

    #[test]
    fn normalize_rounds_every_edge_to_even() {
        assert_eq!(c(641, 361, 1, 61).normalized(1920, 1080), Some(c(640, 360, 0, 60)));
    }

    #[test]
    fn normalize_rejects_a_rectangle_outside_the_source() {
        // The crop cache is plain text and survives a profile change.
        assert_eq!(c(1920, 800, 0, 140).normalized(1280, 720), None);
        assert_eq!(c(640, 360, 700, 0).normalized(1280, 720), None);
        assert_eq!(c(0, 360, 0, 0).normalized(1280, 720), None);
        assert_eq!(c(u32::MAX, 2, 4, 0).normalized(1280, 720), None);
    }

    #[test]
    fn normalize_accepts_a_crop_that_exactly_fills_the_source() {
        assert_eq!(c(1280, 720, 0, 0).normalized(1280, 720), Some(c(1280, 720, 0, 0)));
    }

    #[test]
    fn union_covers_both_samples() {
        // One sample sees bars, another sees content lower down.
        assert_eq!(c(640, 360, 0, 60).union(c(640, 400, 0, 40)), c(640, 400, 0, 40));
        assert_eq!(c(600, 360, 20, 60).union(c(640, 360, 0, 60)), c(640, 360, 0, 60));
    }

    #[test]
    fn to_filter_round_trips() {
        let crop = c(640, 360, 0, 60);
        assert_eq!(crop.to_filter(), "crop=640:360:0:60");
        assert_eq!(Crop::from_str(&crop.to_filter()), Some(crop));
    }

    #[test]
    fn chroma_planes_match_what_a_y4m_reader_expects() {
        use super::{PixelSubsampling, VideoSource};

        // 853x480 4:2:0 needs 427x240 chroma; 426 columns puts every frame 480 bytes short.
        let geom = |ss, w, h| {
            let (cw, ch, _, _) = VideoSource::chroma_geometry(ss, w, h, 0, 0);
            (cw, ch)
        };
        assert_eq!(geom(PixelSubsampling::Yuv420, 853, 480), (427, 240));
        assert_eq!(geom(PixelSubsampling::Yuv420, 1920, 1080), (960, 540));
        assert_eq!(geom(PixelSubsampling::Yuv420, 640, 361), (320, 181));
        assert_eq!(geom(PixelSubsampling::Yuv422, 853, 480), (427, 480));
        assert_eq!(geom(PixelSubsampling::Yuv444, 853, 481), (853, 481));

        // An even crop keeps the chroma offset exact, which is why odd edges are rejected.
        assert_eq!(
            VideoSource::chroma_geometry(PixelSubsampling::Yuv420, 640, 360, 20, 60),
            (320, 180, 10, 30)
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PixelSubsampling {
    Yuv420,
    Yuv422,
    Yuv444,
}

#[derive(Debug, Clone, Copy)]
pub struct PixelFormat {
    pub pix_fmt: c_int,
    pub bit_depth: u32,
    pub subsampling: PixelSubsampling,
}

impl PixelFormat {
    pub fn y4m_colorspace(&self) -> String {
        match (self.subsampling, self.bit_depth) {
            (PixelSubsampling::Yuv420, 8) => "420".into(),
            (PixelSubsampling::Yuv420, 10) => "420p10".into(),
            (PixelSubsampling::Yuv420, 12) => "420p12".into(),
            (PixelSubsampling::Yuv422, 8) => "422".into(),
            (PixelSubsampling::Yuv422, 10) => "422p10".into(),
            (PixelSubsampling::Yuv422, 12) => "422p12".into(),
            (PixelSubsampling::Yuv444, 8) => "444".into(),
            (PixelSubsampling::Yuv444, 10) => "444p10".into(),
            (PixelSubsampling::Yuv444, 12) => "444p12".into(),
            (sub, depth) => {
                tracing::warn!(
                    "unsupported Y4M format ({sub:?}, {depth}-bit) - falling back to 420 8-bit"
                );
                "420".into()
            }
        }
    }

    pub fn bytes_per_sample(&self) -> usize {
        if self.bit_depth > 8 { 2 } else { 1 }
    }
}

pub fn pixfmt_name(sub: PixelSubsampling, depth: u8) -> Option<&'static str> {
    use PixelSubsampling::*;
    Some(match (sub, depth) {
        (Yuv420,  8) => "yuv420p",
        (Yuv420, 10) => "yuv420p10le",
        (Yuv422,  8) => "yuv422p",
        (Yuv422, 10) => "yuv422p10le",
        (Yuv444,  8) => "yuv444p",
        (Yuv444, 10) => "yuv444p10le",
        _ => return None,
    })
}

fn pixfmt_for(sub: PixelSubsampling, depth: u8) -> Option<c_int> {
    pixfmt_name(sub, depth).map(get_pixel_format)
}

/// SVT-AV1 accepts only 8/10-bit 4:2:0.
fn output_shape(decoded: &PixelFormat, opts: OpenOpts) -> (u8, PixelSubsampling) {
    let depth = opts.target_bit_depth.unwrap_or(decoded.bit_depth.min(10) as u8);
    let subsampling = if opts.keep_subsampling { decoded.subsampling } else { PixelSubsampling::Yuv420 };
    (depth, subsampling)
}

pub fn conversion_notes(decoded: &PixelFormat, target_bit_depth: Option<u8>) -> Vec<String> {
    let (depth, subsampling) = output_shape(decoded, OpenOpts { target_bit_depth, keep_subsampling: false });
    let mut notes = Vec::new();
    if u32::from(depth) != decoded.bit_depth {
        notes.push(format!("bit-depth conversion: {}-bit to {depth}-bit", decoded.bit_depth));
    }
    if decoded.subsampling != subsampling {
        notes.push(format!("chroma conversion: {:?} to {subsampling:?}", decoded.subsampling));
    }
    notes
}

#[repr(C)]
struct AVComponentDescriptor {
    plane: c_int,
    step: c_int,
    offset: c_int,
    shift: c_int,
    depth: c_int,
}

// libavutil's AVPixFmtDescriptor, laid out like this since FFmpeg 5.0.
#[repr(C)]
struct AVPixFmtDescriptor {
    name: *const c_char,
    nb_components: u8,
    log2_chroma_w: u8,
    log2_chroma_h: u8,
    flags: u64,
    comp: [AVComponentDescriptor; 4],
    alias: *const c_char,
}

const AV_PIX_FMT_FLAG_BE: u64 = 1 << 0;
const AV_PIX_FMT_FLAG_PAL: u64 = 1 << 1;
const AV_PIX_FMT_FLAG_BITSTREAM: u64 = 1 << 2;
const AV_PIX_FMT_FLAG_HWACCEL: u64 = 1 << 3;
const AV_PIX_FMT_FLAG_PLANAR: u64 = 1 << 4;
const AV_PIX_FMT_FLAG_RGB: u64 = 1 << 5;
const AV_PIX_FMT_FLAG_ALPHA: u64 = 1 << 7;
const AV_PIX_FMT_FLAG_BAYER: u64 = 1 << 8;
const AV_PIX_FMT_FLAG_FLOAT: u64 = 1 << 9;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelLayout {
    pub rgb: bool,
    pub components: u8,
    pub log2_chroma_w: u8,
    pub log2_chroma_h: u8,
    pub depth: u32,
    plain_yuv: bool,
}

impl PixelLayout {
    pub fn not_yuv(&self) -> bool {
        self.rgb || self.components <= 2
    }
}

pub fn pixel_layout_of(name: &str) -> Option<PixelLayout> {
    FFMS_INIT.call_once(|| unsafe { FFMS_Init(0, 0) });
    pixel_layout(get_pixel_format(name))
}

fn pixel_layout(pix_fmt: c_int) -> Option<PixelLayout> {
    type DescGet = unsafe extern "C" fn(c_int) -> *const AVPixFmtDescriptor;
    unsafe extern "C" {
        fn dlsym(handle: *mut std::ffi::c_void, symbol: *const c_char) -> *mut std::ffi::c_void;
    }
    // By name at run time: libffms2 has libavutil in the process, and no build needs FFmpeg's headers for it.
    static DESC_GET: OnceLock<Option<DescGet>> = OnceLock::new();
    let desc_get = (*DESC_GET.get_or_init(|| {
        let symbol = unsafe { dlsym(std::ptr::null_mut(), c"av_pix_fmt_desc_get".as_ptr()) };
        (!symbol.is_null()).then(|| unsafe { std::mem::transmute::<*mut std::ffi::c_void, DescGet>(symbol) })
    }))?;

    let desc = unsafe { desc_get(pix_fmt).as_ref() }?;
    let comp = &desc.comp[..usize::from(desc.nb_components.min(4))];
    let depth = comp.iter().map(|c| c.depth).max().unwrap_or(0).max(0) as u32;
    let odd = AV_PIX_FMT_FLAG_BE | AV_PIX_FMT_FLAG_PAL | AV_PIX_FMT_FLAG_BITSTREAM | AV_PIX_FMT_FLAG_HWACCEL
        | AV_PIX_FMT_FLAG_RGB | AV_PIX_FMT_FLAG_ALPHA | AV_PIX_FMT_FLAG_BAYER | AV_PIX_FMT_FLAG_FLOAT;
    let plain_yuv = desc.flags & AV_PIX_FMT_FLAG_PLANAR != 0
        && desc.flags & odd == 0
        && comp.len() == 3
        && comp.iter().enumerate().all(|(i, c)| c.plane == i as c_int && c.shift == 0 && c.step == if depth > 8 { 2 } else { 1 });
    Some(PixelLayout {
        rgb: desc.flags & AV_PIX_FMT_FLAG_RGB != 0,
        components: desc.nb_components,
        log2_chroma_w: desc.log2_chroma_w,
        log2_chroma_h: desc.log2_chroma_h,
        depth,
        plain_yuv,
    })
}

fn detect_pixel_format(pix_fmt: c_int) -> PixelFormat {
    use PixelSubsampling::*;

    let converted = |subsampling, bit_depth: u8| PixelFormat {
        pix_fmt: pixfmt_for(subsampling, bit_depth).expect("pixfmt_for names every subsampling at 8 and 10 bits"),
        bit_depth: u32::from(bit_depth),
        subsampling,
    };
    let Some(layout) = pixel_layout(pix_fmt) else {
        tracing::warn!("unrecognized FFMS pixel format {pix_fmt} - converting it to 10-bit 4:2:0");
        return converted(Yuv420, 10);
    };
    let subsampling = match (layout.log2_chroma_w, layout.log2_chroma_h) {
        _ if layout.components < 3 => Yuv420,
        (0, _) => Yuv444,
        (_, 0) => Yuv422,
        _ => Yuv420,
    };
    let y4m_shape = matches!((layout.log2_chroma_w, layout.log2_chroma_h), (1, 1) | (1, 0) | (0, 0));
    if layout.plain_yuv && y4m_shape && layout.depth != 9 {
        return PixelFormat { pix_fmt, bit_depth: layout.depth, subsampling };
    }
    // A real format, not the source's own: FFMS2 skips the conversion for a target it
    // already has, and the Y4M header would describe a layout the data does not have.
    converted(subsampling, if layout.depth > 8 { 10 } else { 8 })
}

struct Index(*mut FFMS_Index);

impl Drop for Index {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { FFMS_DestroyIndex(self.0) }
        }
    }
}

static FFMS_INIT: Once = Once::new();

impl Index {
    fn read(index_path: &Path) -> Result<Self> {
        FFMS_INIT.call_once(|| unsafe { FFMS_Init(0, 0) });
        let path_str = index_path.to_str()
            .with_context(|| format!("non-UTF8 index path: {}", index_path.display()))?;
        let cpath = CString::new(path_str)
            .with_context(|| format!("NUL byte in index path: {}", index_path.display()))?;
        let mut ei = ErrorInfo::new();
        let ptr = unsafe { FFMS_ReadIndex(cpath.as_ptr(), &mut ei.raw) };
        if ptr.is_null() {
            bail!("FFMS_ReadIndex failed: {}", ei.message());
        }
        Ok(Index(ptr))
    }

    fn first_video_track(&mut self) -> Result<c_int> {
        let mut ei = ErrorInfo::new();
        let track = unsafe { FFMS_GetFirstTrackOfType(self.0, FFMS_TYPE_VIDEO, &mut ei.raw) };
        if track < 0 {
            bail!("no video track found: {}", ei.message());
        }
        Ok(track)
    }
}

#[derive(Clone)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub sar_num: u32,
    pub sar_den: u32,
    pub num_frames: u32,
    pub pixel_format: PixelFormat,
    pub orientation: Orientation,
}

thread_local! {
    static RESIZED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn take_resized() -> bool {
    RESIZED.replace(false)
}

pub struct VideoSource {
    ptr: *mut FFMS_VideoSource,
    pub info: VideoInfo,
    pub decoded: PixelFormat,
}

// FFMS2 is not thread-safe; each worker owns its own VideoSource on its own thread.
unsafe impl Send for VideoSource {}

impl Drop for VideoSource {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { FFMS_DestroyVideoSource(self.ptr) }
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct OpenOpts {
    pub target_bit_depth: Option<u8>,
    /// Leaves 4:2:2 and 4:4:4 as they are, for a deinterlacer that needs the fields apart.
    pub keep_subsampling: bool,
}

impl VideoSource {
    pub fn open(source_file: &Path, index_file: &Path, opts: OpenOpts) -> Result<Self> {
        Self::open_with(source_file, index_file, opts, FFMS_SEEK_NORMAL)
    }

    pub fn open_linear(source_file: &Path, index_file: &Path, opts: OpenOpts) -> Result<Self> {
        Self::open_with(source_file, index_file, opts, FFMS_SEEK_LINEAR)
    }

    fn open_with(source_file: &Path, index_file: &Path, opts: OpenOpts, seek_mode: c_int) -> Result<Self> {
        let mut idx = Index::read(index_file)?;
        let track = idx.first_video_track()?;

        let src_str = source_file.to_str()
            .with_context(|| format!("non-UTF8 source path: {}", source_file.display()))?;
        let csrc = CString::new(src_str)
            .with_context(|| format!("NUL byte in source path: {}", source_file.display()))?;
        let mut ei = ErrorInfo::new();
        let ptr = unsafe {
            FFMS_CreateVideoSource(csrc.as_ptr(), track, idx.0, 1, seek_mode, &mut ei.raw)
        };
        if ptr.is_null() {
            bail!("FFMS_CreateVideoSource failed: {}", ei.message());
        }

        let mut ei2 = ErrorInfo::new();
        let first_frame = unsafe { FFMS_GetFrame(ptr, 0, &mut ei2.raw) };
        if first_frame.is_null() {
            unsafe { FFMS_DestroyVideoSource(ptr) }
            bail!("FFMS_GetFrame(0) failed: {}", ei2.message());
        }
        let raw_pix_fmt = unsafe { (*first_frame).encoded_pixel_format };
        let decoded = detect_pixel_format(raw_pix_fmt);
        let mut pixel_format = decoded;

        let out_w = unsafe { (*first_frame).encoded_width };
        let out_h = unsafe { (*first_frame).encoded_height };
        let resizer = FFMS_RESIZER_BICUBIC;

        let (depth, subsampling) = output_shape(&decoded, opts);
        if depth as u32 != pixel_format.bit_depth || pixel_format.subsampling != subsampling {
            match pixfmt_for(subsampling, depth) {
                Some(pf) => {
                    pixel_format = PixelFormat { pix_fmt: pf, bit_depth: depth as u32, subsampling };
                }
                None => {
                    unsafe { FFMS_DestroyVideoSource(ptr) }
                    bail!("no pixfmt available for {subsampling:?} at {depth}-bit");
                }
            }
        }

        let target_formats = [pixel_format.pix_fmt, -1i32];
        let mut ei3 = ErrorInfo::new();
        let rc = unsafe {
            FFMS_SetOutputFormatV2(
                ptr,
                target_formats.as_ptr(),
                out_w,
                out_h,
                resizer,
                &mut ei3.raw,
            )
        };
        if rc != 0 {
            unsafe { FFMS_DestroyVideoSource(ptr) }
            bail!("FFMS_SetOutputFormatV2 failed: {}", ei3.message());
        }

        let props_ptr = unsafe { FFMS_GetVideoProperties(ptr) };
        if props_ptr.is_null() {
            unsafe { FFMS_DestroyVideoSource(ptr) }
            bail!("FFMS_GetVideoProperties returned nothing");
        }
        let props = unsafe { &*props_ptr };
        let info = VideoInfo {
            width:      out_w as u32,
            height:     out_h as u32,
            fps_num:    props.fps_numerator as u32,
            fps_den:    props.fps_denominator as u32,
            sar_num:    props.sar_num.max(0) as u32,
            sar_den:    props.sar_den.max(1) as u32,
            num_frames: props.num_frames as u32,
            pixel_format,
            orientation: Orientation::from_ffms2(props.flip, props.rotation),
        };

        Ok(VideoSource { ptr, info, decoded })
    }

    /// Milliseconds per frame, indexed like `FFMS_GetFrame`.
    pub fn timestamps_ms(&self) -> Result<Vec<f64>> {
        let track = unsafe { FFMS_GetTrackFromVideo(self.ptr) };
        if track.is_null() {
            bail!("FFMS_GetTrackFromVideo returned no track");
        }
        let tb_ptr = unsafe { FFMS_GetTimeBase(track) };
        if tb_ptr.is_null() {
            bail!("FFMS_GetTimeBase returned nothing");
        }
        let tb = unsafe { &*tb_ptr };
        (0..unsafe { FFMS_GetNumFrames(track) })
            .map(|n| {
                let info = unsafe { FFMS_GetFrameInfo(track, n) };
                if info.is_null() {
                    bail!("FFMS_GetFrameInfo({n}) returned nothing");
                }
                Ok(unsafe { (*info).pts } as f64 * tb.num as f64 / tb.den as f64)
            })
            .collect()
    }

    /// div_ceil: a Y4M reader takes ceil(w/2) x ceil(h/2) from the header.
    fn chroma_geometry(
        subsampling: PixelSubsampling,
        out_w: usize,
        out_h: usize,
        crop_x: usize,
        crop_y: usize,
    ) -> (usize, usize, usize, usize) {
        match subsampling {
            PixelSubsampling::Yuv420 => {
                (out_w.div_ceil(2), out_h.div_ceil(2), crop_x / 2, crop_y / 2)
            }
            PixelSubsampling::Yuv422 => (out_w.div_ceil(2), out_h, crop_x / 2, crop_y),
            PixelSubsampling::Yuv444 => (out_w, out_h, crop_x, crop_y),
        }
    }

    /// FFMS2's scaled_width is the target even for an unscaled frame; only its scaler's buffer is one av_image_alloc block.
    fn holds_output(frame: &FFMS_Frame, info: &VideoInfo) -> bool {
        let format = &info.pixel_format;
        let (w, h) = (info.width as usize, info.height as usize);
        let same_layout = frame.encoded_pixel_format == format.pix_fmt
            || pixel_layout(frame.encoded_pixel_format).is_some_and(|l| Some(l) == pixel_layout(format.pix_fmt));
        if same_layout && frame.encoded_width as usize == w && frame.encoded_height as usize == h {
            return true;
        }
        let (chroma_w, chroma_h, _, _) = Self::chroma_geometry(format.subsampling, w, h, 0, 0);
        let mut next = frame.data[0] as usize;
        [(w, h), (chroma_w, chroma_h), (chroma_w, chroma_h)].iter().enumerate().all(|(i, &(plane_w, plane_h))| {
            let line = (plane_w * format.bytes_per_sample()).next_multiple_of(4);
            let in_place = frame.data[i] as usize == next && usize::try_from(frame.linesize[i]) == Ok(line);
            next = next.wrapping_add(line * plane_h);
            in_place
        })
    }

    pub fn write_y4m_range<W: Write>(
        &mut self,
        writer: &mut W,
        start: u64,
        end: u64,
        crop: Option<Crop>,
        mut hdr_metadata: Option<&mut Vec<FrameHdrMetadata>>,
    ) -> Result<()> {
        let info = &self.info;
        let cs = info.pixel_format.y4m_colorspace();
        let sar_str = if info.sar_num > 0 && info.sar_den > 0 {
            format!("{}:{}", info.sar_num, info.sar_den)
        } else {
            "0:0".to_string()
        };

        // Raw pointer arithmetic below, so a crop that does not fit reads past the buffer.
        let (out_w, out_h, crop_x, crop_y) = match crop {
            Some(c) => {
                if c.normalized(info.width, info.height) != Some(c) {
                    bail!(
                        "crop {}:{}:{}:{} does not fit source {}x{} (or has odd edges)",
                        c.w, c.h, c.x, c.y, info.width, info.height
                    );
                }
                (c.w as usize, c.h as usize, c.x as usize, c.y as usize)
            }
            None => (info.width as usize, info.height as usize, 0, 0),
        };

        let header = format!(
            "YUV4MPEG2 W{out_w} H{out_h} F{}:{} Ip A{sar_str} C{cs}\n",
            info.fps_num, info.fps_den
        );
        writer.write_all(header.as_bytes()).context("write Y4M header")?;

        let bps = info.pixel_format.bytes_per_sample();

        let (chroma_out_w, chroma_out_h, chroma_cx, chroma_cy) =
            Self::chroma_geometry(info.pixel_format.subsampling, out_w, out_h, crop_x, crop_y);

        let mut ei = ErrorInfo::new();
        for frame_n in start..=end {
            let frame = unsafe { FFMS_GetFrame(self.ptr, frame_n as c_int, &mut ei.raw) };
            if frame.is_null() {
                bail!("FFMS_GetFrame({frame_n}) failed: {}", ei.message());
            }
            let frame_ref = unsafe { &*frame };
            if !Self::holds_output(frame_ref, info) {
                bail!(
                    "FFMS_GetFrame({frame_n}): FFMS2 did not scale this {}x{} frame to the source's {}x{}",
                    frame_ref.encoded_width, frame_ref.encoded_height, info.width, info.height
                );
            }
            if (frame_ref.encoded_width as u32, frame_ref.encoded_height as u32) != (info.width, info.height) {
                RESIZED.set(true);
            }

            writer.write_all(b"FRAME\n").context("write FRAME marker")?;

            if let Some(out) = hdr_metadata.as_deref_mut() {
                out.push(FrameHdrMetadata {
                    dovi_rpu: unsafe { side_data(frame_ref.dolby_vision_rpu, frame_ref.dolby_vision_rpu_size) },
                    hdr10plus: unsafe { side_data(frame_ref.hdr10_plus, frame_ref.hdr10_plus_size) },
                });
            }
            for plane_idx in 0..3usize {
                let plane_data = frame_ref.data[plane_idx];
                let linesize   = frame_ref.linesize[plane_idx];

                let (plane_w, plane_h, px, py) = if plane_idx == 0 {
                    (out_w, out_h, crop_x, crop_y)
                } else {
                    (chroma_out_w, chroma_out_h, chroma_cx, chroma_cy)
                };

                let row_bytes = plane_w * bps;
                let col_off   = px * bps;
                let stride = usize::try_from(linesize).unwrap_or(0);
                if plane_data.is_null() || stride < col_off + row_bytes {
                    bail!(
                        "FFMS_GetFrame({frame_n}): plane {plane_idx} has no data or a line size of {linesize}"
                    );
                }

                for row in 0..plane_h {
                    let src_row = py + row;
                    let ptr = unsafe { plane_data.add(src_row * stride + col_off) };
                    let data = unsafe { std::slice::from_raw_parts(ptr, row_bytes) };
                    writer.write_all(data).context("write frame plane")?;
                }
            }
        }

        writer.flush().context("flush Y4M")
    }
}

pub fn run_ffmsindex(source_file: &Path, index_file: &Path) -> Result<()> {
    // Scratch name: the next run would read a partial file as "reusing existing index".
    let tmp = index_file.with_extension("ffindex.part");
    let _ = std::fs::remove_file(&tmp);

    let mut cmd = std::process::Command::new(external_bin("ffmsindex"));
    cmd.arg("-f").arg(source_file).arg(&tmp);
    let timeout = crate::ext::whole_file_timeout(source_file, 3600);
    let out = crate::ext::output_with_timeout(&mut cmd, timeout, "ffmsindex")?;

    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let _ = std::fs::remove_file(&tmp);
        return Err(crate::ext::tool_error("ffmsindex", out.status, &format!("{stdout}{stderr}")));
    }

    std::fs::rename(&tmp, index_file)
        .with_context(|| format!("rename {} to {}", tmp.display(), index_file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversions_are_named_by_what_reaches_the_encoder() {
        use PixelSubsampling::*;
        let decoded = |bit_depth, subsampling| PixelFormat { pix_fmt: 0, bit_depth, subsampling };
        assert_eq!(conversion_notes(&decoded(12, Yuv422), None), ["bit-depth conversion: 12-bit to 10-bit", "chroma conversion: Yuv422 to Yuv420"]);
        assert_eq!(conversion_notes(&decoded(8, Yuv420), Some(10)), ["bit-depth conversion: 8-bit to 10-bit"]);
        assert_eq!(conversion_notes(&decoded(10, Yuv444), Some(8)), ["bit-depth conversion: 10-bit to 8-bit", "chroma conversion: Yuv444 to Yuv420"]);
        assert!(conversion_notes(&decoded(8, Yuv420), None).is_empty());
        assert!(conversion_notes(&decoded(10, Yuv420), Some(10)).is_empty());
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_frame_struct_is_laid_out_like_ffms_h() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<FFMS_Frame>(), 256);
        assert_eq!(offset_of!(FFMS_Frame, content_light_level_average), 216);
        assert_eq!(offset_of!(FFMS_Frame, dolby_vision_rpu), 224);
        assert_eq!(offset_of!(FFMS_Frame, dolby_vision_rpu_size), 232);
        assert_eq!(offset_of!(FFMS_Frame, hdr10_plus), 240);
        assert_eq!(offset_of!(FFMS_Frame, hdr10_plus_size), 248);
    }

    #[test]
    fn a_format_the_y4m_writer_cannot_take_is_converted_at_its_own_depth_with_no_row_resampled() {
        use PixelSubsampling::*;
        FFMS_INIT.call_once(|| unsafe { FFMS_Init(0, 0) });
        // Passed through, FFMS2 skips swscale and the Y4M header describes the wrong layout.
        for (name, target, depth, subsampling) in [
            ("nv12", "yuv420p", 8, Yuv420), ("nv21", "yuv420p", 8, Yuv420), ("yuv410p", "yuv420p", 8, Yuv420),
            ("gray", "yuv420p", 8, Yuv420), ("gray16le", "yuv420p10le", 10, Yuv420), ("pal8", "yuv420p", 8, Yuv420),
            ("yuva420p", "yuv420p", 8, Yuv420), ("yuv420p9le", "yuv420p10le", 10, Yuv420),
            ("yuv411p", "yuv422p", 8, Yuv422), ("yuyv422", "yuv422p", 8, Yuv422), ("uyvy422", "yuv422p", 8, Yuv422),
            ("yvyu422", "yuv422p", 8, Yuv422), ("nv16", "yuv422p", 8, Yuv422), ("y210le", "yuv422p10le", 10, Yuv422),
            ("yuv422p10be", "yuv422p10le", 10, Yuv422),
            ("yuv440p", "yuv444p", 8, Yuv444), ("yuvj440p", "yuv444p", 8, Yuv444), ("gbrp", "yuv444p", 8, Yuv444),
            ("rgb24", "yuv444p", 8, Yuv444), ("bgr24", "yuv444p", 8, Yuv444), ("gbrp10le", "yuv444p10le", 10, Yuv444),
            ("bgr0", "yuv444p", 8, Yuv444), ("rgba", "yuv444p", 8, Yuv444), ("gbrap12le", "yuv444p10le", 10, Yuv444),
            ("yuva444p10le", "yuv444p10le", 10, Yuv444),
        ] {
            let raw = get_pixel_format(name);
            assert!(raw >= 0, "{name} is unknown to this libavutil");
            let detected = detect_pixel_format(raw);
            assert_ne!(detected.pix_fmt, raw, "{name} reaches the encoder unconverted");
            assert_eq!(detected.pix_fmt, get_pixel_format(target), "{name}");
            assert_eq!((detected.bit_depth, detected.subsampling), (depth, subsampling), "{name}");
        }

        for (known, depth, subsampling) in [
            ("yuv420p", 8, Yuv420), ("yuvj420p", 8, Yuv420), ("yuv420p10le", 10, Yuv420), ("yuv420p12le", 12, Yuv420),
            ("yuv420p16le", 16, Yuv420), ("yuv422p", 8, Yuv422), ("yuvj422p", 8, Yuv422), ("yuv422p10le", 10, Yuv422),
            ("yuv422p12le", 12, Yuv422), ("yuv444p", 8, Yuv444), ("yuvj444p", 8, Yuv444), ("yuv444p10le", 10, Yuv444),
            ("yuv444p12le", 12, Yuv444),
        ] {
            let raw = get_pixel_format(known);
            let detected = detect_pixel_format(raw);
            assert_eq!((detected.pix_fmt, detected.bit_depth, detected.subsampling), (raw, depth, subsampling), "{known}");
        }

        let unknown = detect_pixel_format(-1);
        assert_eq!((unknown.pix_fmt, unknown.bit_depth), (get_pixel_format("yuv420p10le"), 10));
    }

    fn frame_at(width: c_int, height: c_int, pix_fmt: c_int, data: [usize; 3], linesize: [c_int; 3]) -> FFMS_Frame {
        let mut frame: FFMS_Frame = unsafe { std::mem::zeroed() };
        frame.encoded_width = width;
        frame.encoded_height = height;
        frame.encoded_pixel_format = pix_fmt;
        frame.scaled_width = 1280;
        frame.scaled_height = 720;
        for i in 0..3 {
            frame.data[i] = std::ptr::without_provenance(data[i]);
            frame.linesize[i] = linesize[i];
        }
        frame
    }

    fn output_of(pix_fmt: c_int, bit_depth: u32) -> VideoInfo {
        VideoInfo {
            width: 1280, height: 720, fps_num: 24, fps_den: 1, sar_num: 1, sar_den: 1, num_frames: 1,
            pixel_format: PixelFormat { pix_fmt, bit_depth, subsampling: PixelSubsampling::Yuv420 },
            orientation: Orientation::default(),
        }
    }

    #[test]
    fn a_frame_ffms2_left_at_another_size_or_format_is_not_read_as_the_output() {
        FFMS_INIT.call_once(|| unsafe { FFMS_Init(0, 0) });
        let yuv420 = get_pixel_format("yuv420p");
        let info = output_of(yuv420, 8);
        let base = 0x10_0000;
        let scaler_buffer = [base, base + 1280 * 720, base + 1280 * 720 + 640 * 360];

        assert!(VideoSource::holds_output(&frame_at(1280, 720, yuv420, [base, 0x90_0000, 0xA0_0000], [1312, 656, 656]), &info));
        assert!(VideoSource::holds_output(&frame_at(1280, 720, get_pixel_format("yuvj420p"), [base, 0x90_0000, 0xA0_0000], [1312, 656, 656]), &info));
        assert!(VideoSource::holds_output(&frame_at(1280, 64, yuv420, scaler_buffer, [1280, 640, 640]), &info));
        assert!(VideoSource::holds_output(&frame_at(1920, 1080, yuv420, scaler_buffer, [1280, 640, 640]), &info));
        assert!(VideoSource::holds_output(&frame_at(1280, 720, get_pixel_format("yuv444p"), scaler_buffer, [1280, 640, 640]), &info));

        assert!(!VideoSource::holds_output(&frame_at(1280, 64, yuv420, [base, 0x90_0000, 0xA0_0000], [1312, 656, 656]), &info));
        assert!(!VideoSource::holds_output(&frame_at(1920, 720, yuv420, [base, base + 1920 * 720, base + 1920 * 720 + 960 * 360], [1920, 960, 960]), &info));
        assert!(!VideoSource::holds_output(&frame_at(1280, 720, get_pixel_format("yuv422p"), [base, 0x90_0000, 0xA0_0000], [1312, 656, 656]), &info));
    }

    #[test]
    fn the_scaler_buffer_of_a_ten_bit_output_has_two_bytes_a_sample() {
        FFMS_INIT.call_once(|| unsafe { FFMS_Init(0, 0) });
        let info = output_of(get_pixel_format("yuv420p10le"), 10);
        let base = 0x10_0000;
        let buffer = [base, base + 2560 * 720, base + 2560 * 720 + 1280 * 360];
        assert!(VideoSource::holds_output(&frame_at(640, 360, get_pixel_format("yuv420p"), buffer, [2560, 1280, 1280]), &info));
        assert!(!VideoSource::holds_output(&frame_at(640, 360, get_pixel_format("yuv420p"), buffer, [1280, 640, 640]), &info));
    }

    #[test]
    fn a_format_name_with_a_nul_is_no_format() {
        assert_eq!(get_pixel_format("yuv420p\0"), -1);
        assert_eq!(pixel_layout_of("yuv\0420p"), None);
    }

    #[test]
    fn full_width_chroma_and_what_is_not_yuv_are_read_off_the_format_itself() {
        let layout = |name: &str| pixel_layout_of(name).unwrap_or_else(|| panic!("{name} is unknown to this libavutil"));
        for name in ["gbrp", "rgb24", "bgr0", "pal8", "gray", "gray10le", "ya8", "monow", "gbrp12le", "x2rgb10le"] {
            assert!(layout(name).not_yuv(), "{name}");
        }
        for name in ["yuv420p", "yuvj420p", "yuv444p10le", "nv12", "yuyv422", "yuva420p", "yuv440p"] {
            assert!(!layout(name).not_yuv(), "{name}");
        }
        for (name, full_width) in [("yuv444p", true), ("yuv440p", true), ("yuvj440p", true), ("yuv422p", false), ("yuv411p", false), ("yuv420p", false)] {
            assert_eq!(layout(name).log2_chroma_w == 0, full_width, "{name}");
        }
        assert_eq!(pixel_layout_of("no-such-format"), None);
    }

    #[test]
    fn the_display_matrix_is_a_mirror_of_the_pixels_and_a_counter_clockwise_roll() {
        use Mirror::*;
        let o = |mirror, roll| Orientation { mirror, roll };
        for (flip, rotation, expected) in [
            (0, 0, o(None, 0)), (0, 90, o(None, -90)), (0, 180, o(None, 180)), (0, 270, o(None, 90)),
            (1, 0, o(Some(Horizontal), 0)), (1, 90, o(Some(Horizontal), -90)),
            (1, 180, o(Some(Vertical), 0)), (1, 270, o(Some(Horizontal), 90)),
            (-1, 0, o(Some(Vertical), 0)), (-1, 180, o(Some(Horizontal), 0)),
            (0, -90, o(None, 90)), (0, 360, o(None, 0)),
        ] {
            assert_eq!(Orientation::from_ffms2(flip, rotation), expected, "flip {flip}, rotation {rotation}");
        }
    }
}
