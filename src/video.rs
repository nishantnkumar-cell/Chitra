//! Phone videos: recognising them, and reading what the viewer needs to know
//! before playing one (codec, size, rotation, length) from the MP4/QuickTime
//! container. Decoding and playback use the operating system's own player
//! (see the viewer's `video` module); nothing here decodes video.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

/// Extensions of the videos phones record: MP4 (Android, WhatsApp, screen
/// recordings), MOV (iPhone) and 3GP (older phones), lower case.
pub const EXTENSIONS: [&str; 5] = ["mp4", "m4v", "mov", "3gp", "3g2"];

/// True when the path has a phone video extension.
pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// What the container says about a video.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    /// Coded size of the picture, before rotation.
    pub width: u32,
    pub height: u32,
    /// Clockwise quarter turns needed to show the picture upright: phones
    /// store portrait videos sideways and say so here.
    pub quarter_turns: u8,
    pub duration: Option<Duration>,
    /// Sample entry codes, e.g. `avc1` or `hvc1`, and `mp4a`.
    pub video_codec: Option<[u8; 4]>,
    pub audio_codec: Option<[u8; 4]>,
}

impl VideoInfo {
    /// Size as shown, after rotation.
    pub fn display_size(&self) -> [u32; 2] {
        if self.quarter_turns % 2 == 1 {
            [self.height, self.width]
        } else {
            [self.width, self.height]
        }
    }

    pub fn is_hevc(&self) -> bool {
        matches!(&self.video_codec, Some(c) if c == b"hvc1" || c == b"hev1")
    }

    pub fn video_codec_name(&self) -> Option<String> {
        self.video_codec.map(|c| codec_name(&c))
    }

    pub fn audio_codec_name(&self) -> Option<String> {
        self.audio_codec.map(|c| codec_name(&c))
    }
}

/// A readable name for a sample entry code.
pub fn codec_name(code: &[u8; 4]) -> String {
    match code {
        b"avc1" | b"avc3" => "H.264".into(),
        b"hvc1" | b"hev1" => "HEVC (H.265)".into(),
        b"dvh1" | b"dvhe" => "Dolby Vision (HEVC)".into(),
        b"av01" => "AV1".into(),
        b"vp09" => "VP9".into(),
        b"mp4v" => "MPEG-4 Visual".into(),
        b"s263" | b"h263" => "H.263".into(),
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => "Apple ProRes".into(),
        b"mp4a" => "AAC".into(),
        b"ac-3" => "Dolby Digital".into(),
        b"ec-3" => "Dolby Digital Plus".into(),
        b"Opus" => "Opus".into(),
        b"alac" => "Apple Lossless".into(),
        b"samr" => "AMR".into(),
        b"sawb" => "AMR-WB".into(),
        b"lpcm" | b"sowt" | b"twos" => "PCM".into(),
        other => String::from_utf8_lossy(other).trim().to_string(),
    }
}

/// Largest `moov` box read; real ones are a few hundred KB even for long
/// videos, so anything bigger is a damaged or hostile file.
const MAX_MOOV: u64 = 64 << 20;

/// Read the codec, size, rotation and length of an MP4 or QuickTime file.
pub fn probe(path: &Path) -> io::Result<VideoInfo> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let moov = find_moov(&mut file, len)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an MP4 or MOV video"))?;
    Ok(parse_moov(&moov))
}

/// Walk the top-level boxes by their headers (the `mdat` with the actual
/// video is skipped, not read) and return the body of `moov`.
fn find_moov(file: &mut File, len: u64) -> io::Result<Option<Vec<u8>>> {
    let mut pos = 0u64;
    while pos + 8 <= len {
        file.seek(SeekFrom::Start(pos))?;
        let mut head = [0u8; 16];
        let n = read_up_to(file, &mut head)?;
        if n < 8 {
            break;
        }
        let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
        let (size, header) = match size32 {
            0 => (len - pos, 8),
            1 if n >= 16 => (u64::from_be_bytes(head[8..16].try_into().unwrap()), 16),
            1 => break,
            s => (s, 8),
        };
        if size < header {
            break;
        }
        if &head[4..8] == b"moov" {
            let body = size - header;
            if body > MAX_MOOV || pos + size > len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "damaged video header",
                ));
            }
            file.seek(SeekFrom::Start(pos + header))?;
            let mut buf = vec![0; body as usize];
            file.read_exact(&mut buf)?;
            return Ok(Some(buf));
        }
        pos = match pos.checked_add(size) {
            Some(p) => p,
            None => break,
        };
    }
    Ok(None)
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Child boxes of a box body, as (type, body).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let (size, header) = match size32 {
            0 => (data.len(), 8),
            1 => {
                let big = u64::from_be_bytes(data.get(8..16)?.try_into().unwrap());
                (usize::try_from(big).ok()?, 16)
            }
            s => (s, 8),
        };
        if size < header || size > data.len() {
            return None;
        }
        let body = &data[header..size];
        data = &data[size..];
        Some((kind, body))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, b)| b)
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn parse_moov(moov: &[u8]) -> VideoInfo {
    let mut info = VideoInfo {
        duration: child(moov, b"mvhd").and_then(mvhd_duration),
        ..VideoInfo::default()
    };
    for (kind, trak) in boxes(moov) {
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia") else {
            continue;
        };
        let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12));
        let codec = child(mdia, b"minf")
            .and_then(|m| child(m, b"stbl"))
            .and_then(|s| child(s, b"stsd"))
            .and_then(|s| s.get(12..16))
            .and_then(|c| <[u8; 4]>::try_from(c).ok());
        match handler {
            Some(b"vide") if info.video_codec.is_none() => {
                info.video_codec = codec;
                if let Some(tkhd) = child(trak, b"tkhd") {
                    if let Some((w, h, turns)) = tkhd_geometry(tkhd) {
                        info.width = w;
                        info.height = h;
                        info.quarter_turns = turns;
                    }
                }
            }
            Some(b"soun") if info.audio_codec.is_none() => info.audio_codec = codec,
            _ => {}
        }
    }
    info
}

fn mvhd_duration(mvhd: &[u8]) -> Option<Duration> {
    let (scale, duration) = if *mvhd.first()? == 1 {
        (be32(mvhd, 20)?, be64(mvhd, 24)?)
    } else {
        (be32(mvhd, 12)?, be32(mvhd, 16)? as u64)
    };
    if scale == 0 || duration == u64::MAX || duration == u32::MAX as u64 {
        return None;
    }
    Some(Duration::from_secs_f64(duration as f64 / scale as f64))
}

/// Size and clockwise quarter turns from a track header's matrix.
fn tkhd_geometry(tkhd: &[u8]) -> Option<(u32, u32, u8)> {
    // Version/flags, then times, track id and duration (wider in version 1),
    // then 16 bytes of layer/volume fields before the 3x3 matrix.
    let matrix = if *tkhd.first()? == 1 {
        4 + 32 + 16
    } else {
        4 + 20 + 16
    };
    let a = be32(tkhd, matrix)? as i32;
    let b = be32(tkhd, matrix + 4)? as i32;
    let w = be32(tkhd, matrix + 36)? >> 16;
    let h = be32(tkhd, matrix + 40)? >> 16;
    let degrees = (b as f64).atan2(a as f64).to_degrees();
    let turns = ((degrees / 90.0).round() as i32).rem_euclid(4) as u8;
    Some((w, h, turns))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a box from its type and body.
    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn tkhd(a: i32, b: i32, c: i32, d: i32, w: u32, h: u32) -> Vec<u8> {
        let mut t = vec![0u8; 4 + 20 + 16];
        for v in [a, b, 0, c, d, 0, 0, 0, 0x4000_0000] {
            t.extend_from_slice(&v.to_be_bytes());
        }
        t.extend_from_slice(&(w << 16).to_be_bytes());
        t.extend_from_slice(&(h << 16).to_be_bytes());
        bx(b"tkhd", &t)
    }

    fn trak(handler: &[u8; 4], codec: &[u8; 4], tkhd: Vec<u8>) -> Vec<u8> {
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 12]);
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend_from_slice(&bx(codec, &[0; 8]));
        let stbl = bx(b"stbl", &bx(b"stsd", &stsd));
        let mdia = [bx(b"hdlr", &hdlr), bx(b"minf", &stbl)].concat();
        bx(b"trak", &[tkhd, bx(b"mdia", &mdia)].concat())
    }

    fn mvhd(scale: u32, duration: u32) -> Vec<u8> {
        let mut m = vec![0u8; 12];
        m.extend_from_slice(&scale.to_be_bytes());
        m.extend_from_slice(&duration.to_be_bytes());
        m.extend_from_slice(&[0; 80]);
        bx(b"mvhd", &m)
    }

    #[test]
    fn reads_a_portrait_phone_video() {
        let one = 0x1_0000;
        let moov = [
            mvhd(600, 1500),
            trak(b"vide", b"hvc1", tkhd(0, one, -one, 0, 1920, 1080)),
            trak(b"soun", b"mp4a", tkhd(one, 0, 0, one, 0, 0)),
        ]
        .concat();
        let info = parse_moov(&moov);
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.quarter_turns, 1);
        assert_eq!(info.display_size(), [1080, 1920]);
        assert_eq!(info.duration, Some(Duration::from_millis(2500)));
        assert!(info.is_hevc());
        assert_eq!(info.audio_codec_name().as_deref(), Some("AAC"));
    }

    #[test]
    fn rotation_from_matrix() {
        let one = 0x1_0000;
        let turns = |a, b, c, d| tkhd_geometry(&tkhd(a, b, c, d, 4, 2)[8..]).unwrap().2;
        assert_eq!(turns(one, 0, 0, one), 0);
        assert_eq!(turns(0, one, -one, 0), 1);
        assert_eq!(turns(-one, 0, 0, -one), 2);
        assert_eq!(turns(0, -one, one, 0), 3);
    }

    #[test]
    fn finds_moov_after_mdat_and_rejects_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("clip.mov");
        let moov = bx(
            b"moov",
            &trak(b"vide", b"avc1", tkhd(0x1_0000, 0, 0, 0x1_0000, 640, 480)),
        );
        let file = [bx(b"ftyp", b"qt  \0\0\0\0"), bx(b"mdat", &[7; 1000]), moov].concat();
        std::fs::write(&video, file).unwrap();
        let info = probe(&video).unwrap();
        assert_eq!(info.video_codec_name().as_deref(), Some("H.264"));
        assert_eq!(info.display_size(), [640, 480]);

        let text = dir.path().join("notes.mp4");
        std::fs::write(&text, b"hello").unwrap();
        assert!(probe(&text).is_err());
    }

    #[test]
    fn probes_the_test_clips() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let clip = probe(&data.join("h264-aac.mp4")).unwrap();
        assert_eq!(clip.video_codec_name().as_deref(), Some("H.264"));
        assert_eq!(clip.audio_codec_name().as_deref(), Some("AAC"));
        assert_eq!(clip.display_size(), [160, 120]);
        assert_eq!(clip.quarter_turns, 0);
        let secs = clip.duration.unwrap().as_secs_f32();
        assert!((0.9..1.2).contains(&secs), "{secs}");
        // Made with ffmpeg -display_rotation 90 (counter-clockwise).
        let portrait = probe(&data.join("portrait.mov")).unwrap();
        assert_eq!(portrait.quarter_turns, 3);
        assert_eq!(portrait.display_size(), [120, 160]);
        assert!(probe(&data.join("hevc.mov")).unwrap().is_hevc());
    }

    #[test]
    fn recognises_phone_video_names() {
        assert!(is_video_path(Path::new("IMG_0001.MOV")));
        assert!(is_video_path(Path::new("VID_20240101.mp4")));
        assert!(!is_video_path(Path::new("photo.heic")));
        assert!(!is_video_path(Path::new("mp4")));
    }
}
