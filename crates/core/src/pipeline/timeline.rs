use super::Error;
use crate::media::Stream;

#[derive(Copy, Clone)]
pub(super) struct Clock {
    pub den: u64,
}

impl Clock {
    pub fn from_stream(stream: &Stream) -> Result<Self, Error> {
        let (_, den) = stream.raw["time_base"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .and_then(|(n, d)| n.parse::<u64>().ok().zip(d.parse::<u64>().ok()))
            .filter(|(n, d)| *n > 0 && *d > 0)
            .ok_or(Error::UnsupportedMedia(crate::text::en(crate::text::Msg::NoTimeBase)))?;
        Ok(Self { den })
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

#[cfg(test)]
#[test]
fn recognizes_known_dynamic_hdr_labels() {
    assert!(dynamic_hdr("HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"));
    assert!(dynamic_hdr("DOVI RPU Data"));
    assert!(!dynamic_hdr("Content light level metadata"));
}
