//! S04 per-output policy. No encoder or filesystem side effects.
use crate::media::{MediaInfo, Rational, Stream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec { H264, Hevc, Av1 }

impl Codec {
    pub fn label(self) -> &'static str { match self { Self::H264 => "H264", Self::Hevc => "HEVC", Self::Av1 => "AV1" } }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error { UnknownDuration, UnsupportedStreams, UnknownGeometry, UnknownChannels, InsufficientCapacity, RateOverflow }

#[derive(Debug)]
pub struct Plan<'a> {
    pub limit_bytes: u64,
    pub duration_us: u64,
    pub audio_bps: Vec<u64>,
    pub audio_copy: Vec<bool>,
    pub audio_bytes: u64,
    pub mux_reserve_bytes: u64,
    pub video_bytes: u64,
    pub video_bps: u64,
    pub codec: Codec,
    pub width: u64,
    pub height: u64,
    pub low_bpp: bool,
    pub adaptive_downscale: bool,
    /// Retain original timestamps, including VFR. Average is descriptive only.
    pub source_rate: Option<Rational>,
    /// Includes original depth, HDR, color tags, SAR and raw side data.
    pub source: &'a Stream,
}

impl Plan<'_>{
    /// HEVC and AV1 use 10-bit output even for 8-bit SDR input.
    /// Do not hide invalid/unknown source bit depth from the route decision.
    pub fn output_depth(&self)->Option<u64>{
        match (self.codec,self.source.bit_depth){
            (Codec::Hevc|Codec::Av1,Some(8|10))=>Some(10),
            (_,depth)=>depth,
        }
    }
}

/// Only call with the closed, fully muxed output's measured file length.
pub fn fits(limit: u64, measured: Option<u64>) -> bool {
    measured.is_some_and(|bytes| bytes > 0 && bytes <= limit)
}

pub const SHORT_LIMIT_US:u64=600_000_000;
pub fn short_video(duration_us:u64)->bool { duration_us<=SHORT_LIMIT_US }
fn threshold(codec:Codec)->u128 {match codec {Codec::H264=>7,Codec::Hevc=>5,Codec::Av1=>4}}
fn adequate_bpp(codec:Codec,bps:u64,w:u64,h:u64,rate:Option<Rational>,interlaced:bool)->bool {
    rate.is_none_or(|r| {
        let bits = u128::from(bps).saturating_mul(u128::from(r.denominator)).saturating_mul(100);
        let pixels=u128::from(w).saturating_mul(u128::from(h)).saturating_mul(u128::from(r.numerator))
            .saturating_mul(if interlaced{2}else{1}).saturating_mul(threshold(codec));
        bits>=pixels
    })
}
pub fn plan(media: &MediaInfo, limit: u64, codec: Codec, max_long_edge: u32) -> Result<Plan<'_>, Error> {
    let mut video = None;
    let mut audio_bps = Vec::new();
    let mut audio_copy = Vec::new();
    let mut audio_channels = Vec::new();
    for stream in &media.streams {
        match stream.kind.as_deref() {
            Some("video") if !stream.attached_picture && video.is_none() => video = Some(stream),
            Some("audio") => {
                let channels = stream.channels.filter(|n| *n > 0).ok_or(Error::UnknownChannels)?;
                let normal=channels.checked_mul(64_000).ok_or(Error::RateOverflow)?;
                let source_rate=stream.raw["bit_rate"].as_u64().or_else(||stream.raw["bit_rate"].as_str()?.parse().ok());
                // Only copy AAC, which is already compatible with MP4, and only with a known budget.
                let copy=stream.codec.as_deref()==Some("aac") && source_rate.is_some_and(|n|n>0 && n<=normal.min(128_000));
                audio_channels.push(channels);
                audio_copy.push(copy);
                audio_bps.push(if copy {source_rate.unwrap()} else {normal});
            }
            // Production maps the first non-cover video and all audio tracks.
            _ => {}
        }
    }
    let source = video.ok_or(Error::UnsupportedStreams)?;
    let duration = media.duration_us.filter(|d| *d > 0).ok_or(Error::UnknownDuration)?;
    let (w,h) = source.width.zip(source.height).ok_or(Error::UnknownGeometry)?;
    if w < 2 || h < 2 || max_long_edge < 2 { return Err(Error::UnknownGeometry); }

    // Keep all channels/tracks. Reduce AAC rates only under a tight video budget.
    let mux = 4096 + limit.div_ceil(100);
    let usable=limit.checked_sub(mux).ok_or(Error::InsufficientCapacity)?;
    let audio_size=|rates:&[u64],copies:&[bool]|->Result<u64,Error>{
        rates.iter().zip(copies).try_fold(0u64,|sum,(&rate,&copy)| {
            let rate=if copy {u128::from(rate)*105/100}else{u128::from(rate)};
            let bytes=rate.checked_mul(u128::from(duration)).ok_or(Error::InsufficientCapacity)?.div_ceil(8_000_000);
            sum.checked_add(u64::try_from(bytes).map_err(|_|Error::InsufficientCapacity)?).ok_or(Error::InsufficientCapacity)
        })
    };
    let mut audio_bytes=audio_size(&audio_bps,&audio_copy);
    for per_channel in [48_000u64,32_000] {
        if audio_bytes.as_ref().is_ok_and(|&n|n<=usable/3){break;}
        for (i,&channels) in audio_channels.iter().enumerate(){
            let reduced=channels.checked_mul(per_channel).ok_or(Error::RateOverflow)?;
            if reduced<audio_bps[i]{audio_bps[i]=reduced;audio_copy[i]=false;}
        }
        audio_bytes=audio_size(&audio_bps,&audio_copy);
    }
    let audio_bytes=audio_bytes?;
    let video_bytes = limit.checked_sub(mux).and_then(|n| n.checked_sub(audio_bytes))
        .ok_or(Error::InsufficientCapacity)?;
    let rate = u128::from(video_bytes) * 8_000_000 / u128::from(duration);
    let video_bps = u64::try_from(rate).map_err(|_| Error::RateOverflow)?;
    if video_bps == 0 { return Err(Error::InsufficientCapacity); }

    let long=w.max(h);
    let cap=u64::from(max_long_edge).min(long);
    let rate=source.avg_rate.filter(|r|r.numerator>0&&r.denominator>0);
    let interlaced=matches!(source.raw["field_order"].as_str(),Some("tt"|"bb"|"tb"|"bt"));
    let geometry=|edge:u64|->Option<(u64,u64)> {
        if edge==long{return Some((w,h));}
        let scale=|side| ((u128::from(side)*u128::from(edge)/u128::from(long)) as u64)&!1;
        let (x,y)=(scale(w),scale(h));
        (x>=2&&y>=2).then_some((x,y))
    };
    let (mut width,mut height)=geometry(cap).ok_or(Error::UnknownGeometry)?;
    let mut adaptive_downscale=false;
    if !short_video(duration) && rate.is_some() {
        for edge in [3840,2560,1920,1280,960,854,640].into_iter().filter(|&n|n<cap) {
            if adequate_bpp(codec,video_bps,width,height,rate,interlaced){break;}
            if let Some((x,y))=geometry(edge){width=x;height=y;adaptive_downscale=true;}
        }
    }
    let low_bpp=!adequate_bpp(codec,video_bps,width,height,rate,interlaced);
    Ok(Plan { limit_bytes: limit, duration_us: duration, audio_bps, audio_copy, audio_bytes,
        mux_reserve_bytes: mux, video_bytes, video_bps, codec, width, height, low_bpp, adaptive_downscale,
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
    fn output_depth_policy_preserves_hdr_and_promotes_sdr(){
        let mut m=media();m.streams[0].bit_depth=Some(8);m.streams[0].hdr=Hdr::Sdr;
        for (codec,depth) in [(Codec::H264,8),(Codec::Hevc,10),(Codec::Av1,10)]{
            let p=plan(&m,10_000_000,codec,1280).unwrap();
            assert_eq!(p.source.bit_depth,Some(8));assert_eq!(p.output_depth(),Some(depth));
        }
        m.streams[0].bit_depth=Some(10);
        assert_eq!(plan(&m,10_000_000,Codec::H264,1280).unwrap().output_depth(),Some(10));
        m.streams[0].bit_depth=None;
        assert_eq!(plan(&m,10_000_000,Codec::Hevc,1280).unwrap().output_depth(),None);
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
    fn interlaced_1080_to_1280_does_not_drop_to_960() {
        let probe = json!({"format":{"duration":"30"},"streams":[
            {"codec_type":"video","width":1920,"height":1080,"avg_frame_rate":"30000/1001","field_order":"tb","color_transfer":"smpte2084","pix_fmt":"yuv420p10le"},
            {"codec_type":"audio","channels":2}]});
        let m = parse("clip".into(),probe.to_string().as_bytes()).unwrap();
        for codec in [Codec::H264,Codec::Hevc,Codec::Av1] {
            let p = plan(&m,5_117_000,codec,1280).unwrap();
            assert_eq!((p.width,p.height),(1280,720));
        }
    }
    #[test]
    fn long_video_only_and_actual_field_cadence() {
        let mut m=media();m.streams.truncate(1);
        m.streams[0].width=Some(1920);m.streams[0].height=Some(1080);
        m.streams[0].avg_rate=Some(Rational{numerator:30,denominator:1});
        m.duration_us=Some(SHORT_LIMIT_US);
        let short=plan(&m,120_000_000,Codec::Hevc,1280).unwrap();
        assert_eq!((short.width,short.height),(1280,720));
        assert!(!short.adaptive_downscale);
        m.duration_us=Some(SHORT_LIMIT_US+1);
        let long=plan(&m,120_000_000,Codec::Hevc,1280).unwrap();
        assert_eq!((long.width,long.height),(1280,720));
        m.streams[0].raw["field_order"]=json!("tb");
        let fields=plan(&m,120_000_000,Codec::Hevc,1280).unwrap();
        assert!(fields.width<1280 && fields.adaptive_downscale);
        assert!(!short_video(SHORT_LIMIT_US+1));
    }
    #[test]
    fn audio_copy_and_tight_budget() {
        let mut m=media();
        m.streams.truncate(2);
        m.streams[1].codec=Some("aac".into());
        m.streams[1].raw["bit_rate"]=json!("96000");
        let a=plan(&m,10_000_000,Codec::Hevc,1280).unwrap();
        assert_eq!(a.audio_copy,[true]);assert_eq!(a.audio_bps,[96000]);
        let b=plan(&m,1_000_000,Codec::Hevc,1280).unwrap();
        assert_eq!(b.audio_copy,[false]);assert_eq!(b.audio_bps,[64000]);
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
            assert_eq!((p.width,p.height),(2560,1440));
        }
        m.streams[0].width = Some(1071);
        m.streams[0].height = Some(1920);
        let p = plan(&m,2_000_000,Codec::Hevc,1920).unwrap();
        assert_eq!((p.width,p.height),(1071,1920));
    }

}
