//! オンディスクフォーマット共通の符号化プリミティブ (docs/design/storage.md §1)。
//!
//! すべてリトルエンディアン。文字列は u32 長 + UTF-8。
//! チェックサムは CRC32C。

use hamane_core::{HamaneError, MetaValue, Metadata, Metric, Result};

// ファイル種別ごとの magic (8 バイト)
pub const MAGIC_WAL: [u8; 8] = *b"HAMANEW\x01";
pub const MAGIC_VECTORS: [u8; 8] = *b"HAMANEV\x01";
pub const MAGIC_IDS: [u8; 8] = *b"HAMANEI\x01";
pub const MAGIC_META: [u8; 8] = *b"HAMANEM\x01";
pub const MAGIC_TOMBSTONES: [u8; 8] = *b"HAMANET\x01";
pub const MAGIC_MANIFEST: [u8; 8] = *b"HAMANEF\x01";
pub const MAGIC_HNSW: [u8; 8] = *b"HAMANEH\x01";
pub const MAGIC_SQ8: [u8; 8] = *b"HAMANEQ\x01";

pub fn corrupted(msg: impl Into<String>) -> HamaneError {
    HamaneError::Corrupted(msg.into())
}

// ---------------------------------------------------------------------------
// 書き込み (Vec<u8> への追記)
// ---------------------------------------------------------------------------

pub fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_f64(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_f32_slice(out: &mut Vec<u8>, v: &[f32]) {
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

pub fn put_string(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

// ---------------------------------------------------------------------------
// 読み取り (カーソル)
// ---------------------------------------------------------------------------

/// バイト列上のカーソル。範囲外読み取りは `Corrupted` を返す。
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(corrupted(format!(
                "unexpected end of data: need {n} bytes, have {}",
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn f32_vec(&mut self, len: usize) -> Result<Vec<f32>> {
        let bytes = self.take(len * 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }

    pub fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| corrupted("invalid UTF-8 string"))
    }
}

// ---------------------------------------------------------------------------
// CRC32C フレーミング (WAL レコード用)
// ---------------------------------------------------------------------------

/// フレームヘッダ長: crc32c u32 + len u32
pub const FRAME_HEADER_LEN: usize = 8;

/// body (type + payload) をフレームとして追記する。
pub fn put_frame(out: &mut Vec<u8>, body: &[u8]) {
    put_u32(out, crc32c::crc32c(body));
    put_u32(out, body.len() as u32);
    out.extend_from_slice(body);
}

/// フレーム読み取りの結果。
pub enum Frame<'a> {
    /// 完全なフレーム。body と消費バイト数
    Ok { body: &'a [u8], consumed: usize },
    /// 末尾の部分書き込み or CRC 不一致 (ここで読み取りを停止すべき)
    Torn,
}

/// buf 先頭のフレームを読む。
pub fn read_frame(buf: &[u8]) -> Frame<'_> {
    if buf.len() < FRAME_HEADER_LEN {
        return Frame::Torn;
    }
    let crc = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
    if buf.len() < FRAME_HEADER_LEN + len {
        return Frame::Torn;
    }
    let body = &buf[FRAME_HEADER_LEN..FRAME_HEADER_LEN + len];
    if crc32c::crc32c(body) != crc || len == 0 {
        return Frame::Torn;
    }
    Frame::Ok {
        body,
        consumed: FRAME_HEADER_LEN + len,
    }
}

// ---------------------------------------------------------------------------
// Metric ↔ u8
// ---------------------------------------------------------------------------

pub fn metric_to_u8(m: Metric) -> u8 {
    match m {
        Metric::L2 => 0,
        Metric::Cosine => 1,
        Metric::Dot => 2,
    }
}

pub fn metric_from_u8(v: u8) -> Result<Metric> {
    match v {
        0 => Ok(Metric::L2),
        1 => Ok(Metric::Cosine),
        2 => Ok(Metric::Dot),
        _ => Err(corrupted(format!("unknown metric tag: {v}"))),
    }
}

// ---------------------------------------------------------------------------
// MetaValue / Metadata
// ---------------------------------------------------------------------------

const META_TAG_STR: u8 = 0;
const META_TAG_INT: u8 = 1;
const META_TAG_FLOAT: u8 = 2;
const META_TAG_BOOL: u8 = 3;

pub fn put_meta_value(out: &mut Vec<u8>, v: &MetaValue) {
    match v {
        MetaValue::Str(s) => {
            put_u8(out, META_TAG_STR);
            put_string(out, s);
        }
        MetaValue::Int(i) => {
            put_u8(out, META_TAG_INT);
            put_i64(out, *i);
        }
        MetaValue::Float(f) => {
            put_u8(out, META_TAG_FLOAT);
            put_f64(out, *f);
        }
        MetaValue::Bool(b) => {
            put_u8(out, META_TAG_BOOL);
            put_u8(out, *b as u8);
        }
    }
}

pub fn read_meta_value(r: &mut Reader) -> Result<MetaValue> {
    match r.u8()? {
        META_TAG_STR => Ok(MetaValue::Str(r.string()?)),
        META_TAG_INT => Ok(MetaValue::Int(r.i64()?)),
        META_TAG_FLOAT => Ok(MetaValue::Float(r.f64()?)),
        META_TAG_BOOL => match r.u8()? {
            0 => Ok(MetaValue::Bool(false)),
            1 => Ok(MetaValue::Bool(true)),
            v => Err(corrupted(format!("invalid bool byte: {v}"))),
        },
        tag => Err(corrupted(format!("unknown MetaValue tag: {tag}"))),
    }
}

/// Metadata 全体。BTreeMap 由来でキー昇順に書かれるため決定的なバイト列になる。
pub fn put_metadata(out: &mut Vec<u8>, meta: &Metadata) {
    put_u32(out, meta.len() as u32);
    for (key, value) in meta {
        put_string(out, key);
        put_meta_value(out, value);
    }
}

pub fn read_metadata(r: &mut Reader) -> Result<Metadata> {
    let count = r.u32()?;
    let mut meta = Metadata::new();
    for _ in 0..count {
        let key = r.string()?;
        let value = read_meta_value(r)?;
        meta.insert(key, value);
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hamane_core::HamaneError;

    fn sample_metadata() -> Metadata {
        let mut m = Metadata::new();
        m.insert("lang".into(), MetaValue::Str("日本語".into()));
        m.insert("year".into(), MetaValue::Int(-2026));
        m.insert("score".into(), MetaValue::Float(0.25));
        m.insert("public".into(), MetaValue::Bool(true));
        m
    }

    #[test]
    fn metadata_roundtrip() {
        for meta in [Metadata::new(), sample_metadata()] {
            let mut buf = Vec::new();
            put_metadata(&mut buf, &meta);
            let mut r = Reader::new(&buf);
            assert_eq!(read_metadata(&mut r).unwrap(), meta);
            assert!(r.is_empty());
        }
    }

    #[test]
    fn metadata_deterministic_bytes() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        put_metadata(&mut a, &sample_metadata());
        put_metadata(&mut b, &sample_metadata());
        assert_eq!(a, b);
    }

    #[test]
    fn scalar_roundtrip() {
        let mut buf = Vec::new();
        put_u8(&mut buf, 7);
        put_u32(&mut buf, 42);
        put_u64(&mut buf, u64::MAX);
        put_i64(&mut buf, -1);
        put_f64(&mut buf, 1.5);
        put_string(&mut buf, "héllo");
        put_f32_slice(&mut buf, &[1.0, -2.5]);
        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 7);
        assert_eq!(r.u32().unwrap(), 42);
        assert_eq!(r.u64().unwrap(), u64::MAX);
        assert_eq!(r.i64().unwrap(), -1);
        assert_eq!(r.f64().unwrap(), 1.5);
        assert_eq!(r.string().unwrap(), "héllo");
        assert_eq!(r.f32_vec(2).unwrap(), vec![1.0, -2.5]);
        assert!(r.is_empty());
    }

    #[test]
    fn truncated_data_is_corrupted() {
        let mut buf = Vec::new();
        put_string(&mut buf, "hello");
        // 長さプレフィックスより短いデータ
        let mut r = Reader::new(&buf[..buf.len() - 2]);
        assert!(matches!(r.string(), Err(HamaneError::Corrupted(_))));
    }

    #[test]
    fn unknown_tags_are_corrupted() {
        let mut r = Reader::new(&[99]);
        assert!(matches!(
            read_meta_value(&mut r),
            Err(HamaneError::Corrupted(_))
        ));
        assert!(matches!(metric_from_u8(9), Err(HamaneError::Corrupted(_))));
    }

    #[test]
    fn metric_roundtrip() {
        for m in [Metric::L2, Metric::Cosine, Metric::Dot] {
            assert_eq!(metric_from_u8(metric_to_u8(m)).unwrap(), m);
        }
    }

    #[test]
    fn frame_roundtrip_and_torn() {
        let mut buf = Vec::new();
        put_frame(&mut buf, b"abc");
        put_frame(&mut buf, b"defgh");

        // 1 フレーム目
        let Frame::Ok { body, consumed } = read_frame(&buf) else {
            panic!("expected Ok frame");
        };
        assert_eq!(body, b"abc");
        // 2 フレーム目
        let Frame::Ok { body, .. } = read_frame(&buf[consumed..]) else {
            panic!("expected Ok frame");
        };
        assert_eq!(body, b"defgh");

        // 末尾切り詰め: どの長さでも 2 フレーム目は Torn
        for cut in consumed + 1..buf.len() {
            match read_frame(&buf[consumed..cut]) {
                Frame::Torn => {}
                Frame::Ok { .. } => panic!("truncated frame must be Torn (cut={cut})"),
            }
        }

        // CRC 破壊
        let mut broken = buf.clone();
        broken[FRAME_HEADER_LEN] ^= 0xFF; // 1 フレーム目の body を反転
        assert!(matches!(read_frame(&broken), Frame::Torn));
    }
}
