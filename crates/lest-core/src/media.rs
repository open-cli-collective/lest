//! Demo post-production with ffmpeg: one recording in, a deliverable out.
//!
//! - **Composition:** each popup's video replaces the main video for the
//!   interval the popup was open, so a sign-in or consent window appears in
//!   the cut where it happened.
//! - **Auto-cut:** stretches with no user action and no beat longer than
//!   `maxGap` are shortened to `keep`, so waiting disappears and every
//!   action stays.
//! - **Chapters:** a WebVTT chapter track from the labeled beats.
//! - **Stills:** one frame per labeled beat, plus a contact sheet.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::browser::Video;
use crate::report::Beat;

#[derive(Debug, Clone)]
pub struct CutOptions {
    pub max_gap: Duration,
    pub keep: Duration,
    /// Seconds kept before the first beat or action (context) and after
    /// the last one (the payoff).
    pub lead: Duration,
    pub tail: Duration,
}

impl Default for CutOptions {
    fn default() -> Self {
        CutOptions {
            max_gap: Duration::from_secs(4),
            keep: Duration::from_millis(1500),
            lead: Duration::from_millis(1500),
            tail: Duration::from_millis(2500),
        }
    }
}

/// A labeled moment in the finished video.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub marker: String,
    pub label: String,
    /// Milliseconds into the cut.
    pub at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Produced {
    pub video: PathBuf,
    /// Every labeled beat, whether or not its still could be taken.
    pub chapter_list: Vec<Chapter>,
    pub chapters: Option<PathBuf>,
    pub beat_sheet: Option<PathBuf>,
    pub stills: Vec<(Chapter, PathBuf)>,
    pub duration_ms: u64,
    pub raw_duration_ms: u64,
}

/// A piece of one source video placed on the composed timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Index into the videos.
    pub source: usize,
    /// Start and end within that source video, in ms.
    pub from_ms: u64,
    pub to_ms: u64,
}

impl Segment {
    fn len(&self) -> u64 {
        self.to_ms.saturating_sub(self.from_ms)
    }
}

/// The composed timeline before cutting: the main video, with whichever
/// popup was opened most recently shown while it is open (so a popup opened
/// from a popup takes over, and the first resumes when it closes). Times are
/// relative to the main page's start. `durations` are the measured lengths
/// of each video.
pub fn compose(videos: &[Video], durations: &[u64]) -> Vec<Segment> {
    let Some(main) = videos.iter().position(|v| v.role == "main") else { return vec![] };
    let t0 = videos[main].opened_at_ms;
    let main_len = durations[main];
    struct Popup {
        source: usize,
        open: u64,
        close: u64,
        /// Where the popup's video starts on the timeline. A recording
        /// starts a little after the window opens, but ends when it closes,
        /// so it is anchored on its end when the close time is known.
        video_start: u64,
    }
    let popups: Vec<Popup> = videos
        .iter()
        .enumerate()
        .filter(|(i, v)| *i != main && v.role != "main")
        .filter_map(|(i, v)| {
            let open = v.opened_at_ms.saturating_sub(t0).min(main_len);
            let (close, video_start) = match v.closed_at_ms {
                Some(c) => {
                    let close = c.saturating_sub(t0).min(main_len);
                    (close, close.saturating_sub(durations[i]))
                }
                None => ((open + durations[i]).min(main_len), open),
            };
            (close > open).then_some(Popup { source: i, open, close, video_start })
        })
        .collect();
    let mut bounds: Vec<u64> = vec![0, main_len];
    for p in &popups {
        bounds.push(p.open);
        bounds.push(p.close);
    }
    bounds.sort_unstable();
    bounds.dedup();
    let mut out: Vec<Segment> = Vec::new();
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        let active = popups.iter().filter(|p| p.open <= a && p.close >= b).max_by_key(|p| p.open);
        let seg = match active {
            Some(p) => Segment {
                source: p.source,
                from_ms: a.saturating_sub(p.video_start),
                to_ms: b.saturating_sub(p.video_start).min(durations[p.source]),
            },
            None => Segment { source: main, from_ms: a, to_ms: b },
        };
        if seg.to_ms <= seg.from_ms {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.source == seg.source && last.to_ms == seg.from_ms => last.to_ms = seg.to_ms,
            _ => out.push(seg),
        }
    }
    out
}

/// Keeps the parts of `[0, total)` worth watching: around each point of
/// interest (a beat or a user action), shortening any longer stretch with
/// nothing in it. Returns ranges on the composed timeline.
pub fn keep_ranges(points: &[u64], total: u64, opts: &CutOptions) -> Vec<(u64, u64)> {
    let mut pts: Vec<u64> = points.iter().copied().filter(|p| *p <= total).collect();
    pts.sort_unstable();
    pts.dedup();
    if pts.is_empty() {
        return vec![(0, total)];
    }
    let max_gap = opts.max_gap.as_millis() as u64;
    let keep = opts.keep.as_millis() as u64;
    let start = pts[0].saturating_sub(opts.lead.as_millis() as u64);
    let end = (pts[pts.len() - 1] + opts.tail.as_millis() as u64).min(total);
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut seg_start = start;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a > max_gap {
            // Keep the first `keep` after a, then resume shortly before b so
            // the next action is seen coming.
            ranges.push((seg_start, a + keep));
            seg_start = b.saturating_sub(keep / 2).max(a + keep);
        }
    }
    ranges.push((seg_start, end.max(seg_start)));
    ranges.retain(|(a, b)| b > a);
    ranges
}

/// Maps a time on the composed timeline to the cut, or `None` when it was
/// cut out (then the next kept moment is used).
pub fn map_time(t: u64, ranges: &[(u64, u64)]) -> u64 {
    let mut acc = 0;
    for &(a, b) in ranges {
        if t < a {
            return acc;
        }
        if t <= b {
            return acc + (t - a);
        }
        acc += b - a;
    }
    acc
}

/// Splits keep-ranges (composed timeline) across source segments: the list
/// of source pieces to concatenate.
pub fn plan_pieces(segments: &[Segment], ranges: &[(u64, u64)]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut seg_start = 0u64;
    for seg in segments {
        let seg_end = seg_start + seg.len();
        for &(a, b) in ranges {
            let s = a.max(seg_start);
            let e = b.min(seg_end);
            if e > s + 40 {
                out.push(Segment {
                    source: seg.source,
                    from_ms: seg.from_ms + (s - seg_start),
                    to_ms: seg.from_ms + (e - seg_start),
                });
            }
        }
        seg_start = seg_end;
    }
    out
}

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
        && Command::new("ffprobe").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn duration_ms(path: &Path) -> Result<u64, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe: {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim()
        .parse::<f64>()
        .map(|d| (d * 1000.0) as u64)
        .map_err(|_| format!("ffprobe could not read {}", path.display()))
}

fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim()))
    }
}

fn secs(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn vtt_time(ms: u64) -> String {
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

pub fn chapters_vtt(chapters: &[Chapter], total_ms: u64) -> String {
    let mut s = String::from("WEBVTT\n\n");
    for (i, c) in chapters.iter().enumerate() {
        let end = chapters.get(i + 1).map(|n| n.at_ms).unwrap_or(total_ms).max(c.at_ms + 1);
        s.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, vtt_time(c.at_ms), vtt_time(end), c.label));
    }
    s
}

/// Produces the deliverable from a recording.
///
/// `beats` and `actions` are relative to the recorder's start (the same
/// clock as the videos' `opened_at_ms`); `labels` maps beat markers to the
/// demo's labels.
pub fn produce(
    videos: &[Video],
    beats: &[Beat],
    actions: &[u64],
    labels: &[(String, String)],
    opts: &CutOptions,
    out_dir: &Path,
) -> Result<Produced, String> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let main = videos.iter().find(|v| v.role == "main").ok_or("no main recording")?;
    let t0 = main.opened_at_ms;
    let durations: Vec<u64> = videos.iter().map(|v| duration_ms(Path::new(&v.path))).collect::<Result<_, _>>()?;
    let segments = compose(videos, &durations);
    let total: u64 = segments.iter().map(Segment::len).sum();
    let mut points: Vec<u64> = beats.iter().map(|b| b.at_ms.saturating_sub(t0)).collect();
    points.extend(actions.iter().map(|a| a.saturating_sub(t0)));
    // Without beats or actions there is nothing to anchor a cut on: keep
    // the whole take. Otherwise popup openings and closings are moments too.
    let ranges = if points.is_empty() {
        vec![(0, total)]
    } else {
        let mut at = 0;
        for (i, seg) in segments.iter().enumerate() {
            if i > 0 {
                points.push(at);
            }
            at += seg.len();
        }
        keep_ranges(&points, total, opts)
    };
    let pieces = plan_pieces(&segments, &ranges);
    if pieces.is_empty() {
        return Err("nothing to keep in the recording".into());
    }

    // Every piece is scaled and padded to the main video's frame.
    let (w, h) = probe_size(Path::new(&main.path)).unwrap_or((1920, 1080));
    let mut args: Vec<String> = Vec::new();
    for v in videos {
        args.push("-i".into());
        args.push(v.path.clone());
    }
    let main_index = videos.iter().position(|v| v.role == "main").unwrap_or(0);
    // Popup backgrounds are stills taken first and looped as extra inputs;
    // freezing a frame inside the filter graph is unreliable with the
    // variable frame timing of browser recordings.
    let mut next_input = videos.len();
    let mut filter = String::new();
    for (i, p) in pieces.iter().enumerate() {
        let src = &videos[p.source];
        let trim = format!("trim=start={}:end={},setpts=PTS-STARTPTS", secs(p.from_ms), secs(p.to_ms));
        let popup = src.size.filter(|_| p.source != main_index);
        let still = out_dir.join(format!("popup-background-{i}.png"));
        let have_still = popup.is_some()
            && run_ffmpeg(&[
                "-ss".into(),
                secs(src.opened_at_ms.saturating_sub(t0)),
                "-i".into(),
                main.path.clone(),
                "-frames:v".into(),
                "1".into(),
                "-update".into(),
                "1".into(),
                still.display().to_string(),
            ])
            .is_ok();
        match popup {
            // A popup: its window, cropped from the frame it was recorded
            // in, centered over a dimmed still of the page that opened it.
            Some([pw, ph]) if have_still => {
                let dur = secs(p.len());
                args.extend([
                    "-loop".into(),
                    "1".into(),
                    "-framerate".into(),
                    "30".into(),
                    "-t".into(),
                    dur.clone(),
                    "-i".into(),
                    still.display().to_string(),
                ]);
                let bg = next_input;
                next_input += 1;
                // The window as recorded, never larger than the frame.
                let (pw, ph) = (pw.min(w), ph.min(h));
                let factor = (w as f64 * 0.9 / pw as f64).min(h as f64 * 0.85 / ph as f64).min(1.6);
                let target_h = ((ph as f64 * factor) as u32).max(2) & !1;
                filter.push_str(&format!(
                    "[{bg}:v]scale={w}:{h},boxblur=10:2,eq=brightness=-0.10:saturation=0.6,setsar=1[bg{i}];\
[{}:v]{trim},crop={pw}:{ph}:0:0,scale=-2:{target_h},pad=iw+2:ih+2:1:1:color=0x00000040[fg{i}];\
[bg{i}][fg{i}]overlay=(W-w)/2:(H-h)/2:shortest=1,fps=30,setsar=1[v{i}];",
                    p.source
                ));
            }
            _ => filter.push_str(&format!(
                "[{}:v]{trim},scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=white,fps=30,setsar=1[v{i}];",
                p.source
            )),
        }
    }
    for i in 0..pieces.len() {
        filter.push_str(&format!("[v{i}]"));
    }
    filter.push_str(&format!("concat=n={}:v=1:a=0[out]", pieces.len()));
    let video = out_dir.join("demo.mp4");
    args.extend([
        "-filter_complex".into(),
        filter,
        "-map".into(),
        "[out]".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "medium".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
    ]);
    args.push(video.display().to_string());
    // The exact command, shell-quoted, to reproduce or adjust the cut (the
    // popup backgrounds it reads stay next to it).
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    let _ = std::fs::write(out_dir.join("cut-command.txt"), format!("ffmpeg {}\n", quoted.join(" ")));
    run_ffmpeg(&args)?;
    let duration = duration_ms(&video).unwrap_or_else(|_| ranges.iter().map(|(a, b)| b - a).sum());

    let chapters: Vec<Chapter> = beats
        .iter()
        .filter_map(|b| {
            let label = labels.iter().find(|(m, _)| m == &b.marker)?.1.clone();
            Some(Chapter {
                marker: b.marker.clone(),
                label,
                at_ms: map_time(b.at_ms.saturating_sub(t0), &ranges).min(duration),
            })
        })
        .collect();
    let mut produced = Produced {
        video,
        chapters: None,
        beat_sheet: None,
        chapter_list: chapters.clone(),
        stills: Vec::new(),
        duration_ms: duration,
        raw_duration_ms: total,
    };
    if chapters.is_empty() {
        return Ok(produced);
    }
    let vtt = out_dir.join("chapters.vtt");
    std::fs::write(&vtt, chapters_vtt(&chapters, duration)).map_err(|e| e.to_string())?;
    produced.chapters = Some(vtt);

    for (i, c) in chapters.iter().enumerate() {
        // A moment after the beat, once the screen has settled.
        let at = (c.at_ms + 600).min(duration.saturating_sub(100));
        let still = out_dir.join(format!("beat-{:02}-{}.jpg", i + 1, slug(&c.marker)));
        let args: Vec<String> = vec![
            "-ss".into(),
            secs(at),
            "-i".into(),
            produced.video.display().to_string(),
            "-frames:v".into(),
            "1".into(),
            "-q:v".into(),
            "3".into(),
            "-update".into(),
            "1".into(),
            still.display().to_string(),
        ];
        if run_ffmpeg(&args).is_ok() {
            produced.stills.push((c.clone(), still));
        }
    }
    if !produced.stills.is_empty() {
        let cols = produced.stills.len().min(3);
        let sheet = out_dir.join("beat-sheet.jpg");
        let mut args: Vec<String> = Vec::new();
        for (_, s) in &produced.stills {
            args.push("-i".into());
            args.push(s.display().to_string());
        }
        // Tiles keep the video's aspect ratio.
        let tile_h = ((640.0 * h as f64 / w as f64) as u32).max(2) & !1;
        let mut f = String::new();
        for i in 0..produced.stills.len() {
            f.push_str(&format!("[{i}:v]scale=640:{tile_h}[s{i}];"));
        }
        let n = produced.stills.len();
        let layout: Vec<String> =
            (0..n).map(|i| format!("{}_{}", (i % cols) * 640, (i / cols) as u32 * tile_h)).collect();
        if n == 1 {
            f.push_str("[s0]null[out]");
        } else {
            for i in 0..n {
                f.push_str(&format!("[s{i}]"));
            }
            f.push_str(&format!("xstack=inputs={n}:layout={}:fill=white[out]", layout.join("|")));
        }
        args.extend([
            "-filter_complex".into(),
            f,
            "-map".into(),
            "[out]".into(),
            "-frames:v".into(),
            "1".into(),
            "-q:v".into(),
            "3".into(),
            "-update".into(),
            "1".into(),
        ]);
        args.push(sheet.display().to_string());
        if run_ffmpeg(&args).is_ok() {
            produced.beat_sheet = Some(sheet);
        }
    }
    Ok(produced)
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn probe_size(path: &Path) -> Option<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x"])
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (w, h) = s.trim().split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(role: &str, open: u64, close: Option<u64>) -> Video {
        Video { path: format!("{role}.webm"), role: role.into(), opened_at_ms: open, closed_at_ms: close, size: None }
    }

    #[test]
    fn popups_replace_the_main_video_while_open() {
        let videos = [v("main", 100, None), v("popup", 3100, Some(5100))];
        let segs = compose(&videos, &[8000, 2100]);
        // The popup's 2.1s video ends when it closes, so it starts 0.1s
        // into its 2s window.
        assert_eq!(
            segs,
            vec![
                Segment { source: 0, from_ms: 0, to_ms: 3000 },
                Segment { source: 1, from_ms: 100, to_ms: 2100 },
                Segment { source: 0, from_ms: 5000, to_ms: 8000 },
            ]
        );
    }

    #[test]
    fn a_popup_opened_from_a_popup_takes_over_then_hands_back() {
        // Main 0..10s; popup A open 2..8s; popup B (from A) open 4..6s.
        let videos = [v("main", 0, None), v("popup", 2000, Some(8000)), v("popup", 4000, Some(6000))];
        let segs = compose(&videos, &[10_000, 6000, 2000]);
        assert_eq!(
            segs,
            vec![
                Segment { source: 0, from_ms: 0, to_ms: 2000 },
                Segment { source: 1, from_ms: 0, to_ms: 2000 },
                Segment { source: 2, from_ms: 0, to_ms: 2000 },
                Segment { source: 1, from_ms: 4000, to_ms: 6000 },
                Segment { source: 0, from_ms: 8000, to_ms: 10_000 },
            ]
        );
    }

    #[test]
    fn long_idle_stretches_are_shortened() {
        let opts = CutOptions::default();
        // Actions at 2s and 3s, then nothing until a beat at 15s.
        let r = keep_ranges(&[2000, 3000, 15000], 20000, &opts);
        assert_eq!(r, vec![(500, 4500), (14250, 17500)]);
        assert_eq!(map_time(3000, &r), 2500);
        assert_eq!(map_time(15000, &r), 4000 + 750);
        // A moment inside the cut maps to the next kept moment.
        assert_eq!(map_time(10000, &r), 4000);
    }

    #[test]
    fn pieces_follow_segment_boundaries() {
        let segs = vec![Segment { source: 0, from_ms: 0, to_ms: 3000 }, Segment { source: 1, from_ms: 0, to_ms: 2000 }];
        let pieces = plan_pieces(&segs, &[(1000, 4000)]);
        assert_eq!(
            pieces,
            vec![Segment { source: 0, from_ms: 1000, to_ms: 3000 }, Segment { source: 1, from_ms: 0, to_ms: 1000 }]
        );
    }

    #[test]
    fn chapters_are_webvtt() {
        let c = vec![
            Chapter { marker: "a".into(), label: "Home".into(), at_ms: 0 },
            Chapter { marker: "b".into(), label: "Done".into(), at_ms: 61_500 },
        ];
        assert_eq!(
            chapters_vtt(&c, 70_000),
            "WEBVTT\n\n1\n00:00:00.000 --> 00:01:01.500\nHome\n\n2\n00:01:01.500 --> 00:01:10.000\nDone\n\n"
        );
    }
}
