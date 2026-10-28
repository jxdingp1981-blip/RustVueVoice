// wav.rs — 16kHz/单声道/16bit WAV 读写（RIFF 解析 + 标准头写盘）
#![allow(dead_code)]

use std::io::{Read, Seek, SeekFrom};

/// 写 16k/mono/16bit WAV
pub fn write_wav(path: &std::path::Path, samples: &[i16]) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;       // fmt chunk size
    f.write_all(&1u16.to_le_bytes())?;        // PCM
    f.write_all(&1u16.to_le_bytes())?;        // mono
    f.write_all(&16000u32.to_le_bytes())?;    // sample rate
    f.write_all(&32000u32.to_le_bytes())?;    // byte rate
    f.write_all(&2u16.to_le_bytes())?;        // block align
    f.write_all(&16u16.to_le_bytes())?;       // bits
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    let mut bytes = Vec::with_capacity(data_len as usize);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    f.write_all(&bytes)?;
    Ok(())
}

pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
}

/// 读 WAV（标准 PCM），返回 (info, 原始 PCM 字节)
pub fn read_wav(path: &std::path::Path) -> std::io::Result<(WavInfo, Vec<u8>)> {
    let mut f = std::fs::File::open(path)?;
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "不是 WAV 文件"));
    }
    let mut info = WavInfo { sample_rate: 16000, channels: 1, bits: 16 };
    let mut data: Vec<u8> = Vec::new();
    loop {
        let mut ch = [0u8; 8];
        if f.read_exact(&mut ch).is_err() {
            break;
        }
        let id = [ch[0], ch[1], ch[2], ch[3]];
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        match &id {
            b"fmt " => {
                let mut fmt = vec![0u8; size as usize];
                f.read_exact(&mut fmt)?;
                if fmt.len() >= 16 {
                    info.channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                    info.sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                    info.bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                }
            }
            b"data" => {
                let mut buf = vec![0u8; size as usize];
                f.read_exact(&mut buf)?;
                data = buf;
                break;
            }
            _ => {
                let _ = f.seek(SeekFrom::Current(size as i64));
                if size % 2 == 1 {
                    let _ = f.seek(SeekFrom::Current(1));
                }
            }
        }
    }
    Ok((info, data))
}
