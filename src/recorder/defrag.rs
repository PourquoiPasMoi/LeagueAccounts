//! Crash-safe recordings. A game is recorded as fragmented MP4: every fraction
//! of a second is complete on disk, so a crash, a frozen PC or a power cut
//! loses only the last moment. Players, however, must read every fragment of
//! such a file before they show anything (minutes for a long game), so when
//! the recording ends the file is turned into a normal MP4 **in place**: the
//! sample tables are collected from the fragments, one index (`moov`) is
//! written at the end, and the old fragment headers are marked as free space.
//! No picture or sound data is read or rewritten (a 2 GB game takes seconds,
//! where a copy takes minutes on a hard disk).
//!
//! The same function repairs a recording cut short: the incomplete last
//! fragment is dropped and the index is written for what is there.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// What `defragment` did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fixed {
    /// Fragments now covered by the index.
    pub fragments: usize,
    /// Length of the recording.
    pub seconds: f64,
    /// Bytes of an unfinished last fragment that were cut off.
    pub dropped: u64,
}

fn problem(text: &'static str) -> io::Error {
    io::Error::other(text)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    (u64::from(be32(bytes, at)) << 32) | u64::from(be32(bytes, at + 4))
}

/// A box at the top level of the file.
#[derive(Clone, Debug)]
struct TopBox {
    kind: [u8; 4],
    offset: u64,
    size: u64,
    /// The whole box, kept for the small ones that are parsed (`moov`, `moof`).
    data: Option<Vec<u8>>,
}

/// Reads the file a few kilobytes at a time: a fragment header and the header
/// of the data box after it come from one disk read (a long game has
/// thousands of fragments, and the recordings folder may be a hard disk).
struct Reader<'a> {
    file: &'a mut File,
    length: u64,
    start: u64,
    buffer: Vec<u8>,
}

const WINDOW: usize = 8 * 1024;
/// Boxes kept in memory are far smaller than this (a `moov` with every table is a few MB).
const LARGEST_KEPT: u64 = 64 * 1024 * 1024;

impl Reader<'_> {
    fn fetch(&mut self, offset: u64, length: usize) -> io::Result<&[u8]> {
        let inside = offset >= self.start && offset + length as u64 <= self.start + self.buffer.len() as u64;
        if !inside {
            let wanted = (length.max(WINDOW) as u64).min(self.length.saturating_sub(offset)) as usize;
            if wanted < length {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.buffer.resize(wanted, 0);
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.read_exact(&mut self.buffer)?;
            self.start = offset;
        }
        let from = (offset - self.start) as usize;
        Ok(&self.buffer[from..from + length])
    }
}

/// The complete top-level boxes and where they end (anything after that is an
/// unfinished box or not MP4 data).
fn scan(file: &mut File, length: u64) -> io::Result<(Vec<TopBox>, u64)> {
    let mut reader = Reader { file, length, start: 0, buffer: Vec::new() };
    let mut boxes = Vec::new();
    let mut position = 0u64;
    while position + 8 <= length {
        let wanted = if position + 16 <= length { 16 } else { 8 };
        let mut header = [0u8; 16];
        header[..wanted].copy_from_slice(reader.fetch(position, wanted)?);
        let kind = [header[4], header[5], header[6], header[7]];
        if !kind.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b' ') {
            break;
        }
        let (size, head) = match be32(&header, 0) {
            // Reaches the end of the file.
            0 => (length - position, 8),
            1 if wanted == 16 => (be64(&header, 8), 16),
            1 => break,
            size => (u64::from(size), 8),
        };
        if size < head || position + size > length {
            break;
        }
        let data = if matches!(&kind, b"moov" | b"moof") && size <= LARGEST_KEPT { Some(reader.fetch(position, size as usize)?.to_vec()) } else { None };
        boxes.push(TopBox { kind, offset: position, size, data });
        position += size;
    }
    Ok((boxes, position))
}

/// The boxes inside `data` (a box's contents): kind and the whole box.
fn children(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut found = Vec::new();
    let mut position = 0usize;
    while position + 8 <= data.len() {
        let mut size = be32(data, position) as usize;
        if size == 0 {
            size = data.len() - position;
        }
        if size == 1 {
            size = be64(data, position + 8) as usize;
        }
        if size < 8 || position + size > data.len() {
            break;
        }
        found.push(([data[position + 4], data[position + 5], data[position + 6], data[position + 7]], &data[position..position + size]));
        position += size;
    }
    found
}

/// A box's contents (after its header).
fn payload(whole: &[u8]) -> &[u8] {
    let head = if be32(whole, 0) == 1 { 16 } else { 8 };
    whole.get(head..).unwrap_or(&[])
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(data).into_iter().find(|(found, _)| found == kind).map(|(_, whole)| whole)
}

fn boxed(kind: &[u8; 4], contents: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(contents.len() + 16);
    if contents.len() + 8 > u32::MAX as usize {
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&(contents.len() as u64 + 16).to_be_bytes());
    } else {
        out.extend_from_slice(&(contents.len() as u32 + 8).to_be_bytes());
        out.extend_from_slice(kind);
    }
    out.extend_from_slice(contents);
    out
}

#[derive(Clone, Copy, Default)]
struct Defaults {
    description: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

/// One track's sample tables, collected from the fragments.
#[derive(Default)]
struct Track {
    id: u32,
    timescale: u32,
    defaults: Defaults,
    sizes: Vec<u32>,
    /// (samples, duration of each).
    durations: Vec<(u32, u32)>,
    /// (samples, composition offset of each).
    offsets: Vec<(u32, i32)>,
    /// Numbers (from 1) of the samples a player can start from.
    sync: Vec<u32>,
    /// (file offset, samples, sample description) of each run of samples.
    chunks: Vec<(u64, u32, u32)>,
    /// In the track's timescale.
    duration: u64,
    /// Decode time of the first sample.
    first_time: Option<u64>,
}

/// Samples of one track that lie together in the file.
struct Run {
    track: usize,
    time: Option<u64>,
    offset: u64,
    description: u32,
    /// (size, duration, flags, composition offset).
    samples: Vec<(u32, u32, u32, i32)>,
}

const NOT_SYNC: u32 = 0x0001_0000;

/// The runs of a `moof` box at `moof_offset`, and where their data ends.
fn parse_moof(moof: &[u8], moof_offset: u64, tracks: &[Track]) -> Option<(Vec<Run>, u64)> {
    let mut runs = Vec::new();
    let mut data_end = 0u64;
    // Where the previous track fragment's data ended (the default base).
    let mut previous_end = moof_offset;
    let mut first = true;
    for (kind, traf) in children(payload(moof)) {
        if &kind != b"traf" {
            continue;
        }
        let traf = payload(traf);
        let tfhd = payload(find(traf, b"tfhd")?);
        let flags = be32(tfhd, 0) & 0x00ff_ffff;
        let track = tracks.iter().position(|track| track.id == be32(tfhd, 4))?;
        let mut defaults = tracks[track].defaults;
        let mut at = 8;
        let mut base = if flags & 0x2_0000 != 0 || first { moof_offset } else { previous_end };
        if flags & 0x1 != 0 {
            base = be64(tfhd, at);
            at += 8;
        }
        if flags & 0x2 != 0 {
            defaults.description = be32(tfhd, at);
            at += 4;
        }
        if flags & 0x8 != 0 {
            defaults.duration = be32(tfhd, at);
            at += 4;
        }
        if flags & 0x10 != 0 {
            defaults.size = be32(tfhd, at);
            at += 4;
        }
        if flags & 0x20 != 0 {
            defaults.flags = be32(tfhd, at);
        }
        first = false;
        let mut time = find(traf, b"tfdt").map(|tfdt| {
            let tfdt = payload(tfdt);
            if tfdt.first() == Some(&1) { be64(tfdt, 4) } else { u64::from(be32(tfdt, 4)) }
        });
        let mut position = base;
        for (kind, trun) in children(traf) {
            if &kind != b"trun" {
                continue;
            }
            let trun = payload(trun);
            let flags = be32(trun, 0) & 0x00ff_ffff;
            let count = be32(trun, 4) as usize;
            let mut at = 8;
            if flags & 0x1 != 0 {
                position = base.checked_add_signed(i64::from(be32(trun, at) as i32))?;
                at += 4;
            }
            let first_flags = if flags & 0x4 != 0 {
                at += 4;
                Some(be32(trun, at - 4))
            } else {
                None
            };
            let each = [0x100, 0x200, 0x400, 0x800].iter().filter(|bit| flags & **bit != 0).count() * 4;
            if trun.len() < at + count * each {
                return None;
            }
            let mut samples = Vec::with_capacity(count);
            let mut bytes = 0u64;
            for index in 0..count {
                let mut sample = (defaults.size, defaults.duration, defaults.flags, 0i32);
                if flags & 0x100 != 0 {
                    sample.1 = be32(trun, at);
                    at += 4;
                }
                if flags & 0x200 != 0 {
                    sample.0 = be32(trun, at);
                    at += 4;
                }
                if flags & 0x400 != 0 {
                    sample.2 = be32(trun, at);
                    at += 4;
                } else if index == 0 {
                    if let Some(first_flags) = first_flags {
                        sample.2 = first_flags;
                    }
                }
                if flags & 0x800 != 0 {
                    sample.3 = be32(trun, at) as i32;
                    at += 4;
                }
                bytes += u64::from(sample.0);
                samples.push(sample);
            }
            if !samples.is_empty() {
                // Only the fragment's first run starts at the fragment's time.
                runs.push(Run { track, time: time.take(), offset: position, description: defaults.description, samples });
            }
            position += bytes;
            data_end = data_end.max(position);
        }
        previous_end = position;
    }
    Some((runs, data_end))
}

impl Track {
    fn add(&mut self, run: &Run) {
        if let Some(time) = run.time {
            match self.first_time {
                None => self.first_time = Some(time),
                Some(first) => {
                    // A hole between fragments: the last sample lasts until this one.
                    let gap = time.saturating_sub(first + self.duration);
                    if gap > 0 && gap <= u64::from(u32::MAX / 2) {
                        if let Some((count, delta)) = self.durations.pop() {
                            if count > 1 {
                                self.durations.push((count - 1, delta));
                            }
                            self.durations.push((1, delta.saturating_add(gap as u32)));
                            self.duration += gap;
                        }
                    }
                }
            }
        } else if self.first_time.is_none() {
            self.first_time = Some(0);
        }
        self.chunks.push((run.offset, run.samples.len() as u32, run.description.max(1)));
        for (size, duration, flags, offset) in &run.samples {
            self.sizes.push(*size);
            if flags & NOT_SYNC == 0 {
                self.sync.push(self.sizes.len() as u32);
            }
            match self.durations.last_mut() {
                Some((count, delta)) if delta == duration => *count += 1,
                _ => self.durations.push((1, *duration)),
            }
            match self.offsets.last_mut() {
                Some((count, value)) if value == offset => *count += 1,
                _ => self.offsets.push((1, *offset)),
            }
            self.duration += u64::from(*duration);
        }
    }

    /// The sample tables of a normal MP4, in place of the empty ones.
    fn tables(&self) -> Vec<u8> {
        let full = |kind: &[u8; 4], version: u8, body: Vec<u8>| {
            let mut contents = vec![version, 0, 0, 0];
            contents.extend_from_slice(&body);
            boxed(kind, &contents)
        };
        let mut out = Vec::new();
        let mut stts = (self.durations.len() as u32).to_be_bytes().to_vec();
        for (count, delta) in &self.durations {
            stts.extend_from_slice(&count.to_be_bytes());
            stts.extend_from_slice(&delta.to_be_bytes());
        }
        out.extend(full(b"stts", 0, stts));
        if self.offsets.iter().any(|(_, offset)| *offset != 0) {
            let negative = self.offsets.iter().any(|(_, offset)| *offset < 0);
            let mut ctts = (self.offsets.len() as u32).to_be_bytes().to_vec();
            for (count, offset) in &self.offsets {
                ctts.extend_from_slice(&count.to_be_bytes());
                ctts.extend_from_slice(&offset.to_be_bytes());
            }
            out.extend(full(b"ctts", u8::from(negative), ctts));
        }
        if self.sync.len() != self.sizes.len() {
            let mut stss = (self.sync.len() as u32).to_be_bytes().to_vec();
            for number in &self.sync {
                stss.extend_from_slice(&number.to_be_bytes());
            }
            out.extend(full(b"stss", 0, stss));
        }
        // Chunks with the same number of samples share one entry.
        let mut entries: Vec<(u32, u32, u32)> = Vec::new();
        for (index, (_, samples, description)) in self.chunks.iter().enumerate() {
            if entries.last().is_none_or(|(_, last_samples, last_description)| last_samples != samples || last_description != description) {
                entries.push((index as u32 + 1, *samples, *description));
            }
        }
        let mut stsc = (entries.len() as u32).to_be_bytes().to_vec();
        for (first, samples, description) in entries {
            stsc.extend_from_slice(&first.to_be_bytes());
            stsc.extend_from_slice(&samples.to_be_bytes());
            stsc.extend_from_slice(&description.to_be_bytes());
        }
        out.extend(full(b"stsc", 0, stsc));
        let mut stsz = vec![0u8; 4];
        stsz.extend_from_slice(&(self.sizes.len() as u32).to_be_bytes());
        for size in &self.sizes {
            stsz.extend_from_slice(&size.to_be_bytes());
        }
        out.extend(full(b"stsz", 0, stsz));
        let mut co64 = (self.chunks.len() as u32).to_be_bytes().to_vec();
        for (offset, _, _) in &self.chunks {
            co64.extend_from_slice(&offset.to_be_bytes());
        }
        out.extend(full(b"co64", 0, co64));
        out
    }
}

/// `mvhd`, `mdhd` (`middle` = 4: the timescale) or `tkhd` (`middle` = 8: track
/// id and a reserved field) with a new duration; long ones need version 1.
fn with_duration(whole: &[u8], kind: &[u8; 4], middle: usize, duration: u64) -> Vec<u8> {
    let body = payload(whole);
    let version = body.first().copied().unwrap_or(0);
    let times = if version == 1 { 16 } else { 8 };
    let length = if version == 1 { 8 } else { 4 };
    if body.len() < 4 + times + middle + length {
        return whole.to_vec();
    }
    let rest = &body[4 + times + middle + length..];
    let mut out = Vec::with_capacity(body.len() + 12);
    if version == 1 || duration > u64::from(u32::MAX - 1) {
        out.push(1);
        out.extend_from_slice(&body[1..4]);
        if version == 1 {
            out.extend_from_slice(&body[4..4 + times + middle]);
        } else {
            out.extend_from_slice(&u64::from(be32(body, 4)).to_be_bytes());
            out.extend_from_slice(&u64::from(be32(body, 8)).to_be_bytes());
            out.extend_from_slice(&body[12..12 + middle]);
        }
        out.extend_from_slice(&duration.to_be_bytes());
    } else {
        out.extend_from_slice(&body[..4 + times + middle]);
        out.extend_from_slice(&(duration as u32).to_be_bytes());
    }
    out.extend_from_slice(rest);
    boxed(kind, &out)
}

/// The edit list with its lengths filled in: a fragmented file leaves them 0
/// ("as long as the fragments"), which in a normal file means "nothing".
fn with_edits(whole: &[u8], duration: u64) -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, child) in children(payload(whole)) {
        if &kind != b"elst" {
            out.extend_from_slice(child);
            continue;
        }
        let mut body = payload(child).to_vec();
        let version = body.first().copied().unwrap_or(0);
        let entries = be32(&body, 4) as usize;
        let each = if version == 1 { 20 } else { 12 };
        for entry in 0..entries {
            let at = 8 + entry * each;
            if body.len() < at + each {
                break;
            }
            if version == 1 {
                if be64(&body, at) == 0 && be64(&body, at + 8) as i64 >= 0 {
                    body[at..at + 8].copy_from_slice(&duration.to_be_bytes());
                }
            } else if be32(&body, at) == 0 && be32(&body, at + 4) as i32 >= 0 {
                body[at..at + 4].copy_from_slice(&(duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
            }
        }
        out.extend(boxed(b"elst", &body));
    }
    boxed(b"edts", &out)
}

/// The same boxes, with those `change` answers for replaced (an empty answer
/// removes the box).
fn rebuilt(kind: &[u8; 4], whole: &[u8], mut change: impl FnMut(&[u8; 4], &[u8]) -> Option<Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    for (child_kind, child) in children(payload(whole)) {
        match change(&child_kind, child) {
            Some(replacement) => out.extend(replacement),
            None => out.extend_from_slice(child),
        }
    }
    boxed(kind, &out)
}

fn track_id(trak: &[u8]) -> u32 {
    let Some(tkhd) = find(payload(trak), b"tkhd") else { return 0 };
    let tkhd = payload(tkhd);
    be32(tkhd, if tkhd.first() == Some(&1) { 20 } else { 12 })
}

fn timescale(header: &[u8]) -> u32 {
    let body = payload(header);
    be32(body, if body.first() == Some(&1) { 20 } else { 12 })
}

/// The tracks named in a fragmented file's `moov`, with the defaults of its `mvex`.
fn tracks_of(moov: &[u8]) -> Vec<Track> {
    let inside = payload(moov);
    let mut tracks: Vec<Track> = children(inside)
        .into_iter()
        .filter(|(kind, _)| kind == b"trak")
        .map(|(_, trak)| {
            let media = find(payload(trak), b"mdia").and_then(|mdia| find(payload(mdia), b"mdhd")).map_or(0, timescale);
            Track { id: track_id(trak), timescale: media.max(1), ..Track::default() }
        })
        .collect();
    if let Some(mvex) = find(inside, b"mvex") {
        for (kind, trex) in children(payload(mvex)) {
            if &kind != b"trex" {
                continue;
            }
            let trex = payload(trex);
            if let Some(track) = tracks.iter_mut().find(|track| track.id == be32(trex, 4)) {
                track.defaults = Defaults { description: be32(trex, 8), duration: be32(trex, 12), size: be32(trex, 16), flags: be32(trex, 20) };
            }
        }
    }
    tracks
}

/// The index of a normal MP4 made from a fragmented file's `moov` and the
/// collected tables. Returns it with the length in seconds.
fn build_moov(moov: &[u8], tracks: &[Track]) -> (Vec<u8>, f64) {
    let movie_scale = find(payload(moov), b"mvhd").map_or(1000, timescale).max(1);
    let in_movie = |track: &Track| (u128::from(track.duration) * u128::from(movie_scale) / u128::from(track.timescale)) as u64;
    let longest = tracks.iter().map(in_movie).max().unwrap_or(0);
    let rebuilt_moov = rebuilt(b"moov", moov, |kind, child| match kind {
        b"mvhd" => Some(with_duration(child, b"mvhd", 4, longest)),
        b"mvex" => Some(Vec::new()),
        b"trak" => {
            let Some(track) = tracks.iter().find(|track| track.id == track_id(child)) else { return Some(Vec::new()) };
            if track.sizes.is_empty() {
                // Nothing was recorded on it: a track without samples confuses players.
                return Some(Vec::new());
            }
            let length = in_movie(track);
            Some(rebuilt(b"trak", child, |kind, child| match kind {
                b"tkhd" => Some(with_duration(child, b"tkhd", 8, length)),
                b"edts" => Some(with_edits(child, length)),
                b"mdia" => Some(rebuilt(b"mdia", child, |kind, child| match kind {
                    b"mdhd" => Some(with_duration(child, b"mdhd", 4, track.duration)),
                    b"minf" => Some(rebuilt(b"minf", child, |kind, child| {
                        (kind == b"stbl").then(|| {
                            let mut tables = Vec::new();
                            for (kind, table) in children(payload(child)) {
                                if !matches!(&kind, b"stts" | b"ctts" | b"stss" | b"stsc" | b"stsz" | b"stz2" | b"stco" | b"co64") {
                                    tables.extend_from_slice(table);
                                }
                            }
                            tables.extend(track.tables());
                            boxed(b"stbl", &tables)
                        })
                    })),
                    _ => None,
                })),
                _ => None,
            }))
        }
        _ => None,
    });
    (rebuilt_moov, longest as f64 / f64::from(movie_scale))
}

fn rename_box(file: &mut File, top: &TopBox, kind: &[u8; 4]) -> io::Result<()> {
    file.seek(SeekFrom::Start(top.offset + 4))?;
    file.write_all(kind)
}

/// Make everything from `start` to `end` one single `mdat` box (16 bytes at
/// `start`, which must be a box of at least that size that is no longer
/// needed). A player then reads the file's first boxes and jumps straight to
/// the index at `end`. Without it the player steps over every old fragment
/// one by one before it shows anything: thousands of reads, a minute or more
/// on a hard disk (a 33-minute game: 13 000 boxes, 3 000 requests, over 40 s).
/// The samples keep their places; the index points at them by file position.
fn span(file: &mut File, start: u64, end: u64) -> io::Result<()> {
    let mut header = [0u8; 16];
    header[3] = 1;
    header[4..8].copy_from_slice(b"mdat");
    header[8..16].copy_from_slice(&(end - start).to_be_bytes());
    file.seek(SeekFrom::Start(start))?;
    file.write_all(&header)?;
    file.sync_data()
}

/// A recording finished by an older version: a normal MP4, but with its old
/// fragments still as thousands of boxes in a row before the index.
pub fn is_chained(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else { return false };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else { return false };
    let mut position = 0u64;
    let mut header = [0u8; 8];
    let mut read = |position: u64, header: &mut [u8; 8]| position + 8 <= length && file.seek(SeekFrom::Start(position)).is_ok() && file.read_exact(header).is_ok();
    for _ in 0..8 {
        if !read(position, &mut header) {
            return false;
        }
        let size = u64::from(be32(&header, 0));
        match &header[4..8] {
            // One box for all the data already (64-bit size), or a file made in one piece.
            b"mdat" if size == 1 => return false,
            b"mdat" => {
                // A small data box followed by more of them, not by the index.
                let next = position + size;
                return size >= 8 && read(next, &mut header) && matches!(&header[4..8], b"free" | b"mdat" | b"moof");
            }
            b"moov" | b"moof" => return false,
            _ if size < 8 => return false,
            _ => position += size,
        }
    }
    false
}

/// Give a recording finished by an older version one data box (see [`span`]).
/// True when the file was changed. Reads every box header once.
pub fn unchain(path: &Path) -> io::Result<bool> {
    let mut file = File::options().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    let (boxes, _) = scan(&mut file, length)?;
    // The index at the end, and nothing left that announces fragments.
    let Some(index) = boxes.last().filter(|top| &top.kind == b"moov") else { return Ok(false) };
    if index.data.as_deref().is_none_or(|moov| find(payload(moov), b"mvex").is_some()) || boxes.iter().any(|top| &top.kind == b"moof") {
        return Ok(false);
    }
    let Some(first_data) = boxes.iter().position(|top| &top.kind == b"mdat") else { return Ok(false) };
    // The old front index (now free space) right before the data: the single box starts there.
    let Some(start) = boxes[..first_data].iter().rev().take_while(|top| &top.kind == b"free").filter(|top| top.size >= 16).last() else { return Ok(false) };
    span(&mut file, start.offset, index.offset)?;
    Ok(true)
}

/// Turn a fragmented recording into a normal MP4 in place (see the module
/// notes). Safe to run again on a file it was interrupted on.
pub fn defragment(path: &Path) -> io::Result<Fixed> {
    let mut file = File::options().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    let (boxes, mut valid_end) = scan(&mut file, length)?;
    let fragments: Vec<&TopBox> = boxes.iter().filter(|top| &top.kind == b"moof").collect();
    let mut front = None;
    let mut index = None;
    for top in boxes.iter().filter(|top| &top.kind == b"moov") {
        let Some(moov) = top.data.clone() else { continue };
        if find(payload(&moov), b"mvex").is_some() {
            front = Some((top, moov));
            break;
        }
        index = Some(moov);
    }
    let Some((front, moov)) = front else {
        // Stopped after the index was written but before every fragment
        // header was marked: finish that.
        let Some(index) = index.filter(|_| !fragments.is_empty()) else { return Err(problem("not fragmented")) };
        for top in &fragments {
            rename_box(&mut file, top, b"free")?;
        }
        let scale = find(payload(&index), b"mvhd").map_or(1, timescale).max(1);
        let body = find(payload(&index), b"mvhd").map(payload).unwrap_or(&[]);
        let duration = if body.first() == Some(&1) { be64(body, 24) } else { u64::from(be32(body, 16)) };
        return Ok(Fixed { fragments: fragments.len(), seconds: duration as f64 / f64::from(scale), dropped: 0 });
    };
    let mut tracks = tracks_of(&moov);
    let mut used = Vec::new();
    for top in fragments {
        let Some(moof) = top.data.as_deref() else { return Err(problem("fragment header too large")) };
        // A fragment whose data is not all there (the recording stopped while
        // writing it) ends the file.
        let Some((runs, _)) = parse_moof(moof, top.offset, &tracks).filter(|(_, data_end)| *data_end <= valid_end) else {
            valid_end = top.offset;
            break;
        };
        for run in &runs {
            tracks[run.track].add(run);
        }
        used.push(top);
    }
    if tracks.iter().all(|track| track.sizes.is_empty()) {
        return Err(problem("no complete fragment"));
    }
    // Fragment headers after the cut go too.
    if let Some(last) = used.last() {
        if let Some(next) = boxes.iter().find(|top| top.offset > last.offset && &top.kind == b"moof") {
            valid_end = valid_end.min(next.offset);
        }
    }
    let dropped = length - valid_end;
    // The fragment lookup table at the end of a finished file has no use now.
    if let Some(last) = boxes.last().filter(|top| &top.kind == b"mfra" && top.offset + top.size == valid_end) {
        valid_end = last.offset;
    }
    let (new_moov, seconds) = build_moov(&moov, &tracks);
    file.set_len(valid_end)?;
    file.seek(SeekFrom::Start(valid_end))?;
    file.write_all(&new_moov)?;
    file.sync_data()?;
    // From here the file is a normal MP4: one write turns the old front index
    // and every fragment after it into a single data box that ends at the new
    // index. (Until that write lands the file still counts as "in fragments",
    // `is_fragmented`, and this runs again at the next start.)
    if front.size >= 16 {
        span(&mut file, front.offset, valid_end)?;
    } else {
        rename_box(&mut file, front, b"free")?;
        for top in &used {
            rename_box(&mut file, top, b"free")?;
        }
        file.sync_data()?;
    }
    Ok(Fixed { fragments: used.len(), seconds, dropped })
}

/// A recording still in fragments: the front `moov` announces them (`mvex`),
/// or fragment headers were left when the clean-up after the index was cut
/// short. Reads only the first boxes.
pub fn is_fragmented(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else { return false };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else { return false };
    let mut position = 0u64;
    for _ in 0..8 {
        let mut header = [0u8; 8];
        if position + 8 > length || file.seek(SeekFrom::Start(position)).is_err() || file.read_exact(&mut header).is_err() {
            return false;
        }
        let size = u64::from(be32(&header, 0));
        match &header[4..8] {
            b"moov" if (8..16 * 1024 * 1024).contains(&size) && position + size <= length => {
                let mut body = vec![0u8; size as usize];
                return file.seek(SeekFrom::Start(position)).is_ok() && file.read_exact(&mut body).is_ok() && find(payload(&body), b"mvex").is_some();
            }
            b"moof" => return true,
            b"mdat" => return false,
            _ if size < 8 => return false,
            _ => position += size,
        }
    }
    false
}

/// The file's structure, for the test program.
pub fn describe(path: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    let Ok(mut file) = File::open(path) else { return lines };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let Ok((boxes, end)) = scan(&mut file, length) else { return lines };
    lines.push(format!("{length} bytes, complete boxes end at {end}, {} boxes", boxes.len()));
    let name = |kind: &[u8; 4]| String::from_utf8_lossy(kind).into_owned();
    let mut counts: Vec<(String, usize, u64)> = Vec::new();
    for top in &boxes {
        match counts.iter_mut().find(|(kind, _, _)| *kind == name(&top.kind)) {
            Some(entry) => {
                entry.1 += 1;
                entry.2 += top.size;
            }
            None => counts.push((name(&top.kind), 1, top.size)),
        }
    }
    lines.push(counts.iter().map(|(kind, count, bytes)| format!("{kind} x{count} ({bytes} B)")).collect::<Vec<_>>().join(", "));
    lines.push(boxes.iter().take(10).map(|top| format!("{}@{}+{}", name(&top.kind), top.offset, top.size)).collect::<Vec<_>>().join(" "));
    fn tree(data: &[u8], depth: usize, lines: &mut Vec<String>) {
        for (kind, whole) in children(data) {
            let body = payload(whole);
            let mut line = format!("{}{} {}", "  ".repeat(depth), String::from_utf8_lossy(&kind), whole.len());
            match &kind {
                b"mvhd" | b"mdhd" => line += &format!(" v{} timescale {}", body.first().copied().unwrap_or(0), timescale(whole)),
                b"tfhd" => line += &format!(" flags {:06x} track {}", be32(body, 0) & 0xff_ffff, be32(body, 4)),
                b"tfdt" => line += &format!(" time {}", if body.first() == Some(&1) { be64(body, 4) } else { u64::from(be32(body, 4)) }),
                b"trun" => line += &format!(" flags {:06x} samples {}", be32(body, 0) & 0xff_ffff, be32(body, 4)),
                b"trex" => line += &format!(" track {} desc {} dur {} size {} flags {:08x}", be32(body, 4), be32(body, 8), be32(body, 12), be32(body, 16), be32(body, 20)),
                b"elst" => line += &format!(" v{} entries {} first [{} {} {}]", body.first().copied().unwrap_or(0), be32(body, 4), be32(body, 8), be32(body, 12) as i32, be32(body, 16)),
                b"stts" | b"stsz" | b"stsc" | b"stco" | b"co64" | b"stss" | b"ctts" => line += &format!(" entries {}", be32(body, if &kind == b"stsz" { 8 } else { 4 })),
                _ => {}
            }
            lines.push(line);
            if matches!(&kind, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"mvex" | b"moof" | b"traf" | b"edts") {
                tree(body, depth + 1, lines);
            }
        }
    }
    let mut shown = 0;
    for top in &boxes {
        if &top.kind == b"moov" || (&top.kind == b"moof" && shown < 2) {
            shown += usize::from(&top.kind == b"moof");
            if let Some(bytes) = &top.data {
                tree(bytes, 0, &mut lines);
            }
        }
    }
    lines
}
