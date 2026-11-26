//! S04 per-output policy. No encoder or filesystem side effects.
use crate::media::{MediaInfo, Rational, Stream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec { H264, Hevc, Av1 }

#[derive(Debug, PartialEq, Eq)]
pub enum Error { UnknownDuration, UnsupportedStreams, UnknownGeometry, UnknownChannels, InsufficientCapacity, RateOverflow }

#[derive(Debug)]
pub struct Plan<'a> {
    pub limit_bytes: u64,
    pub duration_us: u64,
    pub audio_bps: Vec<u64>,
    pub audio_bytes: u64,
    pub mux_reserve_bytes: u64,
    pub video_bytes: u64,
    pub video_bps: u64,
    pub codec: Codec,
    pub width: u64,
    pub height: u64,
    /// Retain original timestamps, including VFR. Average is descriptive only.
    pub source_rate: Option<Rational>,
    /// Includes original depth, HDR, color tags, SAR and raw side data.
    pub source: &'a Stream,
}

/// Only call with the closed, fully muxed output's measured file length.
pub fn fits(limit: u64, measured: Option<u64>) -> bool {
    measured.is_some_and(|bytes| bytes > 0 && bytes <= limit)
}

pub fn plan(media: &MediaInfo, limit: u64, codec: Codec, max_long_edge: u32) -> Result<Plan<'_>, Error> {
    let mut video = None;
    let mut audio_bps = Vec::new();
    for stream in &media.streams {
        match stream.kind.as_deref() {
            Some("video") if !stream.attached_picture && video.is_none() => video = Some(stream),
            Some("audio") => {
                let channels = stream.channels.filter(|n| *n > 0).ok_or(Error::UnknownChannels)?;
                audio_bps.push(channels.checked_mul(64_000).ok_or(Error::RateOverflow)?);
            }
            // Production maps the first non-cover video and all audio tracks.
            _ => {}
        }
    }
    let source = video.ok_or(Error::UnsupportedStreams)?;
    let duration = media.duration_us.filter(|d| *d > 0).ok_or(Error::UnknownDuration)?;
    let (w,h) = source.width.zip(source.height).ok_or(Error::UnknownGeometry)?;
    if w < 2 || h < 2 || max_long_edge < 2 { return Err(Error::UnknownGeometry); }

    // Reserve per track, rounding up to whole bytes before subtraction.
    let mut audio_bytes = 0u64;
    for &rate in &audio_bps {
        let bytes = (u128::from(rate) * u128::from(duration)).div_ceil(8_000_000);
        let bytes = u64::try_from(bytes).map_err(|_| Error::InsufficientCapacity)?;
        audio_bytes = audio_bytes.checked_add(bytes).ok_or(Error::InsufficientCapacity)?;
    }
    let mux = 4096 + limit.div_ceil(100);
    let video_bytes = limit.checked_sub(mux).and_then(|n| n.checked_sub(audio_bytes))
        .ok_or(Error::InsufficientCapacity)?;
    let rate = u128::from(video_bytes) * 8_000_000 / u128::from(duration);
    let video_bps = u64::try_from(rate).map_err(|_| Error::RateOverflow)?;
    if video_bps == 0 { return Err(Error::InsufficientCapacity); }

    // Preserve source geometry whenever it meets the user's maximum.
    // Only oversized sources enter the existing downscale heuristic.
    // 1/20 bit per pixel per frame is a heuristic, not a quality guarantee.
    let long = w.max(h);
    let cap = u64::from(max_long_edge);
    let rate = source.avg_rate.filter(|r| r.numerator > 0 && r.denominator > 0);
    let (width,height) = if long <= cap {
        (w,h)
    } else {
        let mut edges = std::iter::once(cap).chain([3840,2560,1920,1280,960,640,426,256].into_iter().filter(|&n| n < cap));
        loop {
            let edge = edges.next().ok_or(Error::InsufficientCapacity)?;
            let scale = |side| ((u128::from(side) * u128::from(edge) / u128::from(long)) as u64) & !1;
            let (x,y) = (scale(w),scale(h));
            if x < 2 || y < 2 { return Err(Error::UnknownGeometry); }
            if rate.is_none_or(|r| {
                let needed = u128::from(x) * u128::from(y) * u128::from(r.numerator);
                let available = (u128::from(video_bps) * u128::from(r.denominator)).saturating_mul(20);
                needed <= available
            }) { break (x,y); }
        }
    };
    Ok(Plan { limit_bytes: limit, duration_us: duration, audio_bps, audio_bytes,
        mux_reserve_bytes: mux, video_bytes, video_bps, codec, width, height,
        source_rate: rate, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{parse, Hdr};
    use serde_json::json;

    fn media() -> MediaInfo {
        let probe = json!({"format":{"duration":"60.000001"},"streams":[
            {"codec_type":"video","width":1080,"height":1920,"avg_frame_rate":"24000/1001",
             "color_transfer":"smpte2084","pix_fmt":"yuv420p10le"},
            {"codec_type":"audio","channels":2},{"codec_type":"audio","channels":1}]});
        parse("clip".into(),probe.to_string().as_bytes()).unwrap()
    }
    #[test]
    fn allocation_and_preservation() {
        let m = media();
        let p = plan(&m,10_000_000,Codec::Av1,1280).unwrap();
        assert_eq!((p.width,p.height),(720,1280));
        assert_eq!(p.audio_bps,[128000,64000]);
        assert_eq!(p.audio_bytes,1_440_002);
        assert_eq!(p.mux_reserve_bytes,104096);
        assert_eq!(p.video_bytes+p.audio_bytes+p.mux_reserve_bytes,p.limit_bytes);
        let bits_us = u128::from(p.video_bytes)*8_000_000;
        assert!(u128::from(p.video_bps)*u128::from(p.duration_us)<=bits_us);
        assert!(u128::from(p.video_bps+1)*u128::from(p.duration_us)>bits_us);
        assert_eq!(p.source_rate.unwrap(), Rational{numerator:24000,denominator:1001});
        assert_eq!(p.source.hdr,Hdr::Pq);
        assert_eq!(p.source.bit_depth,Some(10));
        assert_eq!(p.codec,Codec::Av1);
        assert!(fits(p.limit_bytes,Some(p.limit_bytes)));
        for actual in [None,Some(0),Some(p.limit_bytes+1)] { assert!(!fits(p.limit_bytes,actual)); }
    }
    #[test]
    fn limits_and_unknowns() {
        let mut m = media();
        assert_eq!(plan(&m,1,Codec::Av1,1920).unwrap_err(),Error::InsufficientCapacity);
        m.streams.truncate(1);
        let p = plan(&m,2_000_000,Codec::Hevc,4096).unwrap();
        assert_eq!((p.width,p.height),(1080,1920));
        assert_eq!(p.audio_bytes,0);
        m.duration_us=None;
        assert_eq!(plan(&m,u64::MAX,Codec::Av1,1920).unwrap_err(),Error::UnknownDuration);
        m.duration_us=Some(1);
        assert_eq!(plan(&m,u64::MAX,Codec::Av1,1920).unwrap_err(),Error::RateOverflow);
        m.duration_us=Some(u64::MAX);
        let p=plan(&m,u64::MAX,Codec::H264,1920).unwrap();
        assert_eq!(p.video_bytes+p.mux_reserve_bytes,u64::MAX);
        m.streams[0].avg_rate=None;
        assert!(plan(&m,u64::MAX,Codec::H264,4096).unwrap().source_rate.is_none());
    }
    #[test]
    fn preserve_geometry_within_cap_even_when_bpp_is_low() {
        let mut m = media();
        m.streams.truncate(1);
        let video = &mut m.streams[0];
        video.width = Some(3840);
        video.height = Some(2160);
        video.avg_rate = Some(Rational{numerator:30,denominator:1});
        m.duration_us = Some(238_570_000);
        for codec in [Codec::H264,Codec::Hevc,Codec::Av1] {
            for cap in [3840,4096] {
                let p = plan(&m,251_382_000,codec,cap).unwrap();
                assert!(p.video_bps < 12_441_600);
                assert_eq!((p.width,p.height),(3840,2160));
            }
            let p = plan(&m,251_382_000,codec,2560).unwrap();
            assert_eq!((p.width,p.height),(2560,1440));
            let p = plan(&m,10_000_000,codec,2560).unwrap();
            assert!(p.width < 2560);
        }
        m.streams[0].width = Some(1071);
        m.streams[0].height = Some(1920);
        let p = plan(&m,2_000_000,Codec::Hevc,1920).unwrap();
        assert_eq!((p.width,p.height),(1071,1920));
    }

}
