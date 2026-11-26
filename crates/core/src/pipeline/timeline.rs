use super::{EncodeRequest, Error, check_cancel, io_error};
use crate::{
    media::{MediaInfo, Stream},
    process::{self, Request, Stop},
};
use std::{
    collections::BTreeSet,
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader},
    sync::atomic::AtomicBool,
};

#[derive(Copy, Clone)]
pub(super) struct Clock {
    num: u64,
    pub den: u64,
}

impl Clock {
    pub fn from_stream(stream: &Stream) -> Result<Self, Error> {
        let (num, den) = stream.raw["time_base"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .and_then(|(n, d)| n.parse::<u64>().ok().zip(d.parse::<u64>().ok()))
            .filter(|(n, d)| *n > 0 && *d > 0)
            .ok_or(Error::UnsupportedMedia("missing stream time base"))?;
        Ok(Self { num, den })
    }

    pub fn movie_timescale(self) -> u64 {
        // The edit list uses the movie clock. Make it an exact multiple
        // of the video clock, while retaining precision for audio offsets.
        [1_000_000, 1_000, 1]
            .into_iter()
            .map(|scale| self.den * (scale / gcd(self.den.into(), scale.into()) as u64))
            .find(|&scale| scale <= i32::MAX as u64)
            .unwrap_or(self.den)
    }

    fn convert(self, ticks: i64, scale: u64) -> Option<i64> {
        let numerator = i128::from(ticks)
            .checked_mul(i128::from(self.num))?
            .checked_mul(i128::from(scale))?;
        let denominator = i128::from(self.den);
        let rounded = numerator.checked_add(numerator.signum() * (denominator / 2))?;
        i64::try_from(rounded / denominator).ok()
    }
}

pub(super) fn dar(stream: &Stream) -> Result<(u128, u128), Error> {
    let ratio = stream.raw["sample_aspect_ratio"].as_str().unwrap_or("1:1");
    let (w, h) = stream
        .width
        .zip(stream.height)
        .ok_or(Error::UnsupportedMedia("missing geometry"))?;
    let (n, d) = match ratio {
        "N/A" | "0:1" => (1, 1),
        s => s
            .split_once(':')
            .and_then(|(n, d)| n.parse::<u64>().ok().zip(d.parse::<u64>().ok()))
            .filter(|(n, d)| *n > 0 && *d > 0)
            .ok_or(Error::UnsupportedMedia("invalid sample aspect ratio"))?,
    };
    let num = u128::from(w) * u128::from(n);
    let den = u128::from(h) * u128::from(d);
    let divisor = gcd(num, den);
    Ok((num / divisor, den / divisor))
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

pub(super) fn dynamic_hdr(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "dovi",
        "dolby vision",
        "hdr10+",
        "smpte2094",
        "smpte 2094",
        "hdr dynamic",
        "hdr vivid",
        "dynamic hdr",
        "cuva",
        "st2094",
    ]
    .iter()
    .any(|pattern| name.contains(pattern))
}

#[derive(Default)]
struct Video {
    timestamps: DefaultHasher,
    prefix: DefaultHasher,
    frames: usize,
    last: Option<i64>,
    end: Option<i64>,
    static_hdr: BTreeSet<String>,
    prefix_hdr: BTreeSet<String>,
    tags: BTreeSet<String>,
    prefix_tags: BTreeSet<String>,
    chroma: BTreeSet<String>,
    prefix_chroma: BTreeSet<String>,
}

struct Audio {
    rate: u64,
    channels: Option<u64>,
    layout: Option<String>,
    first: Option<i64>,
    end: Option<i64>,
    samples: u64,
    gaps: u64,
}

impl Audio {
    fn accept(&mut self, start: i64, samples: u64) -> Option<()> {
        let end = start.checked_add(i64::try_from(samples).ok()?)?;
        if let Some(previous) = self.end {
            // Demuxer timestamp quantization can give adjacent frames slightly
            // different rounded boundaries. Count larger discontinuities.
            if previous.abs_diff(start) > self.rate.div_ceil(500) {
                self.gaps = self.gaps.checked_add(1)?;
            }
        }
        self.first.get_or_insert(start);
        self.end = Some(end);
        self.samples = self.samples.checked_add(samples)?;
        Some(())
    }

    fn matches(&self, output: &Self) -> bool {
        // AAC has one frame of priming or tail padding. MP4 edit lists normally
        // remove priming from the decoded timeline, but padding can remain.
        let tolerance = 1024 + self.rate.div_ceil(500);
        let close = |a: Option<i64>, b: Option<i64>| {
            a.zip(b).is_some_and(|(a, b)| a.abs_diff(b) <= tolerance)
        };
        self.channels == output.channels
            && self.rate == output.rate
            && self
                .layout
                .as_ref()
                .is_none_or(|layout| output.layout.as_ref() == Some(layout))
            && self.gaps == output.gaps
            && self.samples.abs_diff(output.samples) <= tolerance
            && close(self.first, output.first)
            && close(self.end, output.end)
    }
}

pub(super) struct Summary {
    video: Video,
    audio: Vec<Audio>,
}

impl Summary {
    pub fn video_frames(&self) -> usize {
        self.video.frames
    }

    pub fn require_continuous_audio(&self) -> Result<(), Error> {
        if self.audio.iter().any(|track| track.gaps != 0) {
            return Err(Error::UnsupportedMedia(
                "audio has discontinuous timestamps",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn static_hdr_count(&self) -> usize {
        self.video.static_hdr.len()
    }

    pub fn compare(&self, out: &Self, sample: bool) -> Result<(), String> {
        let (frames, timestamps, hdr, tags, chroma) = if sample {
            (
                self.video.frames.min(super::SAMPLE_FRAMES),
                self.video.prefix.finish(),
                &self.video.prefix_hdr,
                &self.video.prefix_tags,
                &self.video.prefix_chroma,
            )
        } else {
            (
                self.video.frames,
                self.video.timestamps.finish(),
                &self.video.static_hdr,
                &self.video.tags,
                &self.video.chroma,
            )
        };
        if frames != out.video.frames || timestamps != out.video.timestamps.finish() {
            return Err(format!(
                "video timestamp sequence changed ({frames} vs {} frames)",
                out.video.frames
            ));
        }
        if !sample
            && self
                .video
                .end
                .zip(out.video.end)
                .is_some_and(|(a, b)| a != b)
        {
            return Err(format!(
                "last video frame duration changed ({:?} vs {:?})",
                self.video.end, out.video.end
            ));
        }
        if hdr != &out.video.static_hdr
            || tags != &out.video.tags
            || (!chroma.is_empty() && chroma != &out.video.chroma)
        {
            return Err(format!(
                "decoded frame HDR metadata or color tags changed: {hdr:?} {tags:?} vs {:?} {:?}",
                out.video.static_hdr, out.video.tags
            ));
        }
        if !sample
            && (self.audio.len() != out.audio.len()
                || !self.audio.iter().zip(&out.audio).all(|(a, b)| a.matches(b)))
        {
            return Err("audio layout or decoded synchronization changed".into());
        }
        Ok(())
    }
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split('|')
        .filter_map(|field| field.split_once('='))
        .find_map(|(key, value)| (key == name && value != "N/A").then_some(value))
}

fn inspect_side_data(line: &str) -> Result<BTreeSet<String>, Error> {
    let mut records = BTreeSet::new();
    let mut current = String::new();
    for token in line.split('|') {
        // FFprobe versions use nested compact sections or fully qualified
        // keys such as side_datum/mastering_display_metadata:red_x.
        let token = token.rsplit_once(':').map_or(token, |(prefix, value)| {
            if prefix.starts_with("side_dat") {
                value
            } else {
                token
            }
        });
        if let Some(kind) = token.strip_prefix("side_data_type=") {
            if !current.is_empty() {
                records.insert(std::mem::take(&mut current));
            }
            if dynamic_hdr(kind) {
                return Err(Error::UnsupportedDynamicHdr);
            }
            if matches!(
                kind,
                "Mastering display metadata" | "Content light level metadata"
            ) {
                current.push_str(token);
            }
        } else if !current.is_empty()
            && token.split_once('=').is_some_and(|(key, _)| {
                matches!(
                    key,
                    "red_x"
                        | "red_y"
                        | "green_x"
                        | "green_y"
                        | "blue_x"
                        | "blue_y"
                        | "white_point_x"
                        | "white_point_y"
                        | "min_luminance"
                        | "max_luminance"
                        | "max_content"
                        | "max_average"
                )
            })
        {
            current.push('|');
            current.push_str(token);
        }
    }
    if !current.is_empty() {
        records.insert(current);
    }
    Ok(records)
}

fn summarize(reader: impl BufRead, media: &MediaInfo, scale: u64) -> Result<Summary, Error> {
    let clocks = media
        .streams
        .iter()
        .map(Clock::from_stream)
        .collect::<Result<Vec<_>, _>>()?;
    let mut audio = media
        .streams
        .iter()
        .filter(|s| s.kind.as_deref() == Some("audio"))
        .map(|s| {
            let rate = s
                .sample_rate
                .filter(|r| *r > 0)
                .ok_or(Error::UnsupportedMedia("missing audio sample rate"))?;
            Ok(Audio {
                rate,
                channels: s.channels,
                layout: s.raw["channel_layout"].as_str().map(str::to_owned),
                first: None,
                end: None,
                samples: 0,
                gaps: 0,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut video = Video::default();
    let mut last_video = false;
    for line in reader.lines() {
        let line = line.map_err(Error::Io)?;
        let side = inspect_side_data(&line)?;
        if line.starts_with("packet|") {
            last_video = false;
        }
        if line.starts_with("frame|") {
            let (index, tick) = field(&line, "stream_index")
                .and_then(|s| s.parse::<u64>().ok())
                .zip(field(&line, "pts").and_then(|s| s.parse::<i64>().ok()))
                .ok_or(Error::UnsupportedMedia(
                    "decoded frame lacks an explicit timestamp",
                ))?;
            let position = media
                .streams
                .iter()
                .position(|s| s.index == Some(index))
                .ok_or(Error::UnsupportedMedia("unknown decoded stream index"))?;
            let clock = clocks[position];
            last_video = media.streams[position].kind.as_deref() == Some("video");
            if last_video {
                let ts = clock
                    .convert(tick, scale)
                    .ok_or(Error::UnsupportedMedia("video timestamp overflow"))?;
                if video.last.is_some_and(|last| ts <= last) {
                    return Err(Error::UnsupportedMedia(
                        "non-monotonic video presentation timestamps",
                    ));
                }
                video.last = Some(ts);
                video.end = field(&line, "duration")
                    .or_else(|| field(&line, "pkt_duration"))
                    .and_then(|s| s.parse::<i64>().ok())
                    .filter(|&n| n > 0)
                    .and_then(|d| tick.checked_add(d))
                    .and_then(|end| clock.convert(end, scale));
                video.frames += 1;
                ts.hash(&mut video.timestamps);
                if video.frames <= super::SAMPLE_FRAMES {
                    ts.hash(&mut video.prefix);
                }
                let tags = [
                    "color_range",
                    "color_space",
                    "color_primaries",
                    "color_transfer",
                ]
                .map(|key| {
                    field(&line, key)
                        .filter(|v| !matches!(*v, "unknown" | "unspecified"))
                        .unwrap_or("")
                })
                .join("|");
                if video.frames <= super::SAMPLE_FRAMES {
                    video.prefix_tags.insert(tags.clone());
                }
                video.tags.insert(tags);
                if let Some(location) = field(&line, "chroma_location")
                    .filter(|v| !matches!(*v, "unknown" | "unspecified"))
                {
                    video.chroma.insert(location.to_owned());
                    if video.frames <= super::SAMPLE_FRAMES {
                        video.prefix_chroma.insert(location.to_owned());
                    }
                }
            } else {
                let audio_index = media.streams[..position]
                    .iter()
                    .filter(|s| s.kind.as_deref() == Some("audio"))
                    .count();
                let track = audio
                    .get_mut(audio_index)
                    .ok_or(Error::UnsupportedMedia("unexpected decoded stream"))?;
                let samples = field(&line, "nb_samples")
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or(Error::UnsupportedMedia("missing decoded sample count"))?;
                clock
                    .convert(tick, track.rate)
                    .and_then(|t| track.accept(t, samples))
                    .ok_or(Error::UnsupportedMedia("audio timestamp overflow"))?;
            }
        }
        if last_video {
            for record in side {
                if video.frames <= super::SAMPLE_FRAMES {
                    video.prefix_hdr.insert(record.clone());
                }
                video.static_hdr.insert(record);
            }
            if video
                .static_hdr
                .iter()
                .filter(|r| r.starts_with("side_data_type=Mastering"))
                .count()
                > 1
                || video
                    .static_hdr
                    .iter()
                    .filter(|r| r.starts_with("side_data_type=Content"))
                    .count()
                    > 1
                || video.tags.len() > 1
                || video.chroma.len() > 1
            {
                return Err(Error::UnsupportedMedia(
                    "changing frame color or static HDR metadata",
                ));
            }
        }
    }
    if video.frames == 0 || audio.iter().any(|track| track.samples == 0) {
        return Err(Error::UnsupportedMedia("no decoded media frames"));
    }
    Ok(Summary { video, audio })
}

pub(super) fn scan(
    request: &EncodeRequest<'_>,
    media: &MediaInfo,
    clock: Clock,
    cancel: &AtomicBool,
) -> Result<Summary, Error> {
    let mut req = Request::new(request.ffprobe).args([
        "-v", "error", "-show_frames", "-show_packets", "-show_entries",
        "frame=stream_index,pts,duration,pkt_duration,nb_samples,color_range,color_space,color_primaries,color_transfer,chroma_location:frame_side_data=side_data_type,red_x,red_y,green_x,green_y,blue_x,blue_y,white_point_x,white_point_y,min_luminance,max_luminance,max_content,max_average:packet=stream_index:packet_side_data=side_data_type",
        "-of", "compact=p=1:nk=0", "-i",
    ]).args([media.path.as_os_str()]);
    req.timeout = request.timeout;
    let (out, summary) = process::run_with_stdout(&req, cancel, |stdout| {
        summarize(BufReader::new(stdout), media, clock.den)
    })
    .map_err(|e| io_error(e, cancel))?;
    check_cancel(cancel)?;
    if out.stop == Stop::TimedOut {
        return Err(Error::Process("frame validation deadline exceeded".into()));
    }
    let summary = summary?;
    if !out.status.success() || !out.stderr.bytes.is_empty() {
        return Err(Error::Process(format!(
            "frame validation: {}",
            String::from_utf8_lossy(&out.stderr.bytes)
        )));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dynamic_hdr_from_legacy_and_qualified_probe_fields() {
        for line in [
            "frame|stream_index=0|pts=0|side_data|side_data_type=HDR Dynamic Metadata SMPTE2094-40 (HDR10+)",
            "packet|stream_index=0|side_datum/dovi_rpu:side_data_type=DOVI RPU Data",
        ] {
            assert!(matches!(
                inspect_side_data(line),
                Err(Error::UnsupportedDynamicHdr)
            ));
        }
        let legacy = "frame|side_data|side_data_type=Content light level metadata|max_content=1000|max_average=400";
        let qualified = "frame|side_datum/content_light_level_metadata:side_data_type=Content light level metadata|side_datum/content_light_level_metadata:max_content=1000|side_datum/content_light_level_metadata:max_average=400";
        let record = inspect_side_data(legacy).unwrap();
        assert_eq!(record.len(), 1);
        assert_eq!(record, inspect_side_data(qualified).unwrap());
    }
}
