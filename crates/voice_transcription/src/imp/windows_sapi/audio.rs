use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use windows::Win32::Media::Audio::{WAVE_FORMAT_PCM, WAVEFORMATEX};
use windows::Win32::Media::Speech::{ISpStream, SPFM_OPEN_READONLY, SpStream};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows_core::{GUID, PCWSTR};

use super::sapi;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
pub struct AudioFormat {
    sample_rate: u32,
    bits_per_sample: u16,
    channels: u16,
}

const SPDFID_WAVE_FORMAT_EX: GUID = GUID::from_u128(0xc31adbae_527f_4ff5_a230_f62bb61ff70c);

impl AudioFormat {
    pub fn from_wav_file(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path)?;
        Self::from_wav_reader(&mut file)
    }

    fn from_wav_reader(reader: &mut (impl Read + Seek)) -> Result<Self> {
        let mut riff_header = [0u8; 12];
        reader.read_exact(&mut riff_header)?;
        if &riff_header[0..4] != b"RIFF" || &riff_header[8..12] != b"WAVE" {
            return Err(Error::UnsupportedWavFormat(
                "missing RIFF/WAVE header".into(),
            ));
        }

        loop {
            let mut chunk_header = [0u8; 8];
            if reader.read_exact(&mut chunk_header).is_err() {
                return Err(Error::UnsupportedWavFormat("missing fmt chunk".into()));
            }

            let chunk_id = &chunk_header[0..4];
            let chunk_size =
                u32::from_le_bytes(chunk_header[4..8].try_into().expect("u32 chunk size"));
            if chunk_id == b"fmt " {
                return Self::from_wav_fmt_chunk(reader, chunk_size);
            }

            let padding = chunk_size % 2;
            reader.seek(SeekFrom::Current(i64::from(chunk_size + padding)))?;
        }
    }

    fn from_wav_fmt_chunk(reader: &mut impl Read, chunk_size: u32) -> Result<Self> {
        if chunk_size < 16 {
            return Err(Error::UnsupportedWavFormat("fmt chunk is too small".into()));
        }

        let mut fmt = vec![0u8; chunk_size as usize];
        reader.read_exact(&mut fmt)?;

        let format_tag = u16::from_le_bytes(fmt[0..2].try_into().expect("u16 format tag"));
        if format_tag != WAVE_FORMAT_PCM as u16 {
            return Err(Error::UnsupportedWavFormat(format!(
                "only PCM WAV is supported, got format tag {format_tag}"
            )));
        }

        let channels = u16::from_le_bytes(fmt[2..4].try_into().expect("u16 channels"));
        let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().expect("u32 sample rate"));
        let bits_per_sample =
            u16::from_le_bytes(fmt[14..16].try_into().expect("u16 bits per sample"));

        if channels == 0 || sample_rate == 0 || bits_per_sample == 0 {
            return Err(Error::UnsupportedWavFormat(
                "zero-valued PCM format field".into(),
            ));
        }

        Ok(Self {
            sample_rate,
            bits_per_sample,
            channels,
        })
    }

    fn to_wave_format(self) -> WAVEFORMATEX {
        let block_align = self.channels * (self.bits_per_sample / 8);
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: self.channels,
            nSamplesPerSec: self.sample_rate,
            nAvgBytesPerSec: self.sample_rate * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: self.bits_per_sample,
            cbSize: 0,
        }
    }
}

pub struct AudioStream {
    intf: ISpStream,
}

impl AudioStream {
    pub fn open_file(path: &Path, format: &AudioFormat) -> Result<Self> {
        let intf: ISpStream = sapi("CoCreateInstance(SpStream)", unsafe {
            CoCreateInstance(&SpStream, None, CLSCTX_ALL)
        })?;
        let wave_format = format.to_wave_format();
        let wide_path = path_to_wide_null(path);

        unsafe {
            sapi(
                "ISpStream::BindToFile",
                intf.BindToFile(
                    PCWSTR::from_raw(wide_path.as_ptr()),
                    SPFM_OPEN_READONLY,
                    Some(&SPDFID_WAVE_FORMAT_EX),
                    Some(&wave_format),
                    0,
                ),
            )?;
        }

        Ok(Self { intf })
    }

    pub fn to_sapi(&self) -> ISpStream {
        self.intf.clone()
    }
}

#[cfg(windows)]
fn path_to_wide_null(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(not(windows))]
fn path_to_wide_null(path: &Path) -> Vec<u16> {
    path.to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect()
}
