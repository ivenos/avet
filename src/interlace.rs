use anyhow::{Context, Result};
use std::path::Path;

use crate::ext::external_bin;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldOrder {
    Tff,
    Bff,
}

impl FieldOrder {
    /// One frame out per frame in, so frame numbers, chunks and timestamps stay as they are.
    pub fn filter(self) -> &'static str {
        match self {
            FieldOrder::Tff => "bwdif=mode=send_frame:parity=tff:deint=all",
            FieldOrder::Bff => "bwdif=mode=send_frame:parity=bff:deint=all",
        }
    }

    fn name(self) -> &'static str {
        match self {
            FieldOrder::Tff => "tff",
            FieldOrder::Bff => "bff",
        }
    }
}

/// A stream flagged interlaced whose frames idet finds combed. Cached in the job's temp dir.
pub fn detect(source: &Path, duration_secs: f64, cache: &Path, stem: &str) -> Result<Option<FieldOrder>> {
    if let Ok(cached) = std::fs::read_to_string(cache) {
        return Ok([FieldOrder::Tff, FieldOrder::Bff].into_iter().find(|o| o.name() == cached.trim()));
    }
    if !flagged_interlaced(source)? {
        return Ok(None);
    }

    let mut total = Counts::default();
    for pct in [20u64, 50, 80] {
        let seek = (duration_secs * pct as f64 / 100.0) as u64;
        let c = run_idet(source, seek)?;
        total = Counts { tff: total.tff + c.tff, bff: total.bff + c.bff, progressive: total.progressive + c.progressive };
    }
    let order = total.verdict();
    let (tff, bff, progressive) = (total.tff, total.bff, total.progressive);
    match order {
        Some(o) => tracing::info!(
            "[{stem}] interlaced, {} first (idet: {tff} tff, {bff} bff, {progressive} progressive): deinterlacing with bwdif",
            if o == FieldOrder::Tff { "top field" } else { "bottom field" }
        ),
        None => tracing::info!(
            "[{stem}] flagged interlaced, but the frames are not (idet: {tff} tff, {bff} bff, {progressive} progressive)"
        ),
    }
    crate::resume::write_atomic(cache, order.map_or("progressive", FieldOrder::name).as_bytes())?;
    Ok(order)
}

#[derive(Debug, Default, PartialEq)]
struct Counts {
    tff: u64,
    bff: u64,
    progressive: u64,
}

impl Counts {
    /// idet reads sharp progressive motion as either field order at random; real fields come
    /// in one order. Static interlaced frames count as progressive.
    fn verdict(&self) -> Option<FieldOrder> {
        let (major, minor, order) = if self.tff >= self.bff {
            (self.tff, self.bff, FieldOrder::Tff)
        } else {
            (self.bff, self.tff, FieldOrder::Bff)
        };
        (major >= 20 && major >= 4 * minor).then_some(order)
    }
}

fn flagged_interlaced(source: &Path) -> Result<bool> {
    #[derive(serde::Deserialize)]
    struct Probe { #[serde(default)] streams: Vec<Stream> }
    #[derive(serde::Deserialize)]
    struct Stream { field_order: Option<String> }

    let probe: Probe = crate::ext::ffprobe_json(
        &["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=field_order", "-of", "json"],
        source,
    )
    .context("probe the field order")?;
    Ok(probe.streams.first()
        .and_then(|s| s.field_order.as_deref())
        .is_some_and(|f| matches!(f, "tt" | "bb" | "tb" | "bt")))
}

fn run_idet(source: &Path, seek_secs: u64) -> Result<Counts> {
    let mut cmd = std::process::Command::new(external_bin("ffmpeg"));
    cmd.args(["-hide_banner", "-nostdin", "-ss", &seek_secs.to_string()])
        .arg("-i").arg(source)
        .args(["-map", "0:v:0", "-frames:v", "200", "-vf", "idet", "-f", "null", "-"]);
    let out = crate::ext::output_with_timeout(&mut cmd, 300, "ffmpeg idet")?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(crate::ext::tool_error(&format!("ffmpeg idet at {seek_secs}s"), out.status, &stderr));
    }
    Ok(parse_idet(&stderr))
}

/// The last summary: ffmpeg can build the filter graph twice and report an empty one first.
fn parse_idet(stderr: &str) -> Counts {
    let Some(line) = stderr.lines().rev().find(|l| l.contains("Multi frame detection:")) else {
        return Counts::default();
    };
    let count = |label: &str| {
        line.split(label).nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    Counts { tff: count("TFF:"), bff: count("BFF:"), progressive: count("Progressive:") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_idet_summary_is_read() {
        let stderr = "[Parsed_idet_0 @ 0x1] Multi frame detection: TFF:     0 BFF:     0 Progressive:     0 Undetermined:     0\n\
                      [Parsed_idet_0 @ 0x2] Single frame detection: TFF:    90 BFF:     3 Progressive:     8 Undetermined:     0\n\
                      [Parsed_idet_0 @ 0x2] Multi frame detection: TFF:   101 BFF:     0 Progressive:     2 Undetermined:     0\n";
        assert_eq!(parse_idet(stderr), Counts { tff: 101, bff: 0, progressive: 2 });
        assert_eq!(parse_idet("no summary"), Counts::default());
    }

    #[test]
    fn fields_in_one_order_are_interlaced_and_a_random_mix_is_not() {
        let counts = |tff, bff, progressive| Counts { tff, bff, progressive };
        assert_eq!(counts(101, 0, 0).verdict(), Some(FieldOrder::Tff));
        assert_eq!(counts(3, 280, 17).verdict(), Some(FieldOrder::Bff));
        assert_eq!(counts(60, 2, 400).verdict(), Some(FieldOrder::Tff));

        assert_eq!(counts(38, 40, 23).verdict(), None);
        assert_eq!(counts(0, 0, 600).verdict(), None);
        assert_eq!(counts(12, 0, 580).verdict(), None);
    }
}
