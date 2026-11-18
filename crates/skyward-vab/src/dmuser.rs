//! The two protocols a compressed snapshot is served through.
//!
//! A partition whose update was written as a compressed snapshot is not read
//! by the kernel's own `dm-snapshot` target: the phone's `snapuserd` serves the
//! blocks, and the kernel asks it through a `dm-user` device. Two protocols
//! meet there, and both are written down here so that they can be tried
//! without a phone.
//!
//! The first is the kernel's: a request is a [`Header`], forty bytes, and the
//! answer is the same header with its type changed and the blocks after it.
//! The second is the controlling side's: plain text over a socket, one line a
//! message, which the daemon answers in words.
//!
//! Both were read from AOSP's `system/core/fs_mgr/libsnapshot/snapuserd`, and
//! the daemon this phone ships was held against the second of them: of the
//! thirteen messages it knows, twelve are in that binary verbatim.

use anyhow::{bail, Result};

/// The kernel asking for blocks.
pub const REQ_MAP_READ: u64 = 0;
/// The kernel writing blocks, which a read-only mount never asks for.
pub const REQ_MAP_WRITE: u64 = 1;
/// The answer carrying the blocks asked for.
pub const RESP_SUCCESS: u64 = 0;
/// The answer carrying no blocks.
pub const RESP_ERROR: u64 = 1;
/// The answer for a request this daemon does not serve.
pub const RESP_UNSUPPORTED: u64 = 2;

/// Five eight-byte fields, packed: `seq`, `type`, `flags`, `sector`, `len`.
pub const HEADER_LEN: usize = 40;

/// What one request or answer through a `dm-user` device says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Which request this answers; the kernel's own bookkeeping.
    pub seq: u64,
    /// [`REQ_MAP_READ`], [`REQ_MAP_WRITE`], or one of the `RESP_` answers.
    pub kind: u64,
    pub flags: u64,
    /// Where the blocks are, in 512-byte sectors.
    pub sector: u64,
    /// How many sectors, and so how many blocks, the answer carries.
    pub len: u64,
}

/// What the kernel asks a daemon for.
///
/// The numbers are the same as [`Answered`]'s: a `0` is a read when the
/// kernel sends it and a success when the daemon answers, and a `1` is a
/// write or an error. The type of a header cannot be read without knowing
/// which side sent it, so the two are told apart by which way it went rather
/// than by its number alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Blocks to be read out of the snapshot.
    Read,
    /// Blocks to be written, which a read-only mount never asks for.
    Write,
}

/// What a daemon answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The blocks the kernel asked for follow the header.
    Success,
    /// No blocks: the request could not be served.
    Error,
    /// This daemon does not serve that request.
    Unsupported,
}

impl Asked {
    fn of(kind: u64) -> Option<Asked> {
        match kind {
            REQ_MAP_READ => Some(Asked::Read),
            REQ_MAP_WRITE => Some(Asked::Write),
            _ => None,
        }
    }

}

impl Answered {
    fn of(kind: u64) -> Option<Answered> {
        match kind {
            RESP_SUCCESS => Some(Answered::Success),
            RESP_ERROR => Some(Answered::Error),
            RESP_UNSUPPORTED => Some(Answered::Unsupported),
            _ => None,
        }
    }

    fn kind(self) -> u64 {
        match self {
            Answered::Success => RESP_SUCCESS,
            Answered::Error => RESP_ERROR,
            Answered::Unsupported => RESP_UNSUPPORTED,
        }
    }
}

impl Header {
    /// The header these forty bytes carry.
    pub fn decode(bytes: &[u8]) -> Result<Header> {
        if bytes.len() < HEADER_LEN {
            bail!("a dm-user header is {HEADER_LEN} bytes, not {}", bytes.len());
        }
        let field = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"));
        Ok(Header { seq: field(0), kind: field(8), flags: field(16), sector: field(24), len: field(32) })
    }

    /// The forty bytes this header is written as.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        for (at, value) in [self.seq, self.kind, self.flags, self.sector, self.len].into_iter().enumerate() {
            out[at * 8..at * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// The request this header makes, where the kernel sent it.
    pub fn asked(&self) -> Option<Asked> {
        Asked::of(self.kind)
    }

    /// The answer this header is, where a daemon sent it.
    pub fn answering(&self) -> Option<Answered> {
        Answered::of(self.kind)
    }

    /// The answer to this request, carrying `len` sectors of blocks.
    pub fn answer(&self, what: Answered, len: u64) -> Header {
        Header { seq: self.seq, kind: what.kind(), flags: 0, sector: self.sector, len }
    }

    /// Whether the blocks it names fit in `bytes`.
    pub fn fits_in(&self, bytes: usize) -> bool {
        HEADER_LEN as u64 + self.len.saturating_mul(SECTOR) <= bytes as u64
    }
}

/// The size of a sector, which is what `len` counts.
pub const SECTOR: u64 = 512;

/// What the controlling side says to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Whether the daemon is `active` or `passive`.
    Query,
    /// Give `<misc_name>` its copy-on-write device and the device it reads
    /// through: `init,<misc>,<cow>,<backing>[,<merge target>]`.
    Init { misc: String, cow: String, backing: String, merge_target: Option<String> },
    /// Start serving `<misc>`.
    Start { misc: String },
    /// Let go of a device.
    Delete { device: String },
    /// Begin merging `<misc>` in the daemon rather than in the kernel.
    InitiateMerge { misc: String },
    /// How far the merge has come, as a percentage.
    MergePercent,
    /// What `<misc>` is doing.
    Status { misc: String },
    /// Whether the blocks written verify.
    VerifyUpdate,
    PauseMerge,
    ResumeMerge,
    Stop,
    Detach,
}

impl Request {
    /// The line this request is sent as: the words, commas between them, and
    /// no ending — the daemon reads a message, not a line.
    pub fn text(&self) -> String {
        match self {
            Request::Query => "query".into(),
            Request::Init { misc, cow, backing, merge_target } => match merge_target {
                Some(target) => format!("init,{misc},{cow},{backing},{target}"),
                None => format!("init,{misc},{cow},{backing}"),
            },
            Request::Start { misc } => format!("start,{misc}"),
            Request::Delete { device } => format!("delete,{device}"),
            Request::InitiateMerge { misc } => format!("initiate_merge,{misc}"),
            Request::MergePercent => "merge_percent".into(),
            Request::Status { misc } => format!("getstatus,{misc}"),
            Request::VerifyUpdate => "update-verify".into(),
            Request::PauseMerge => "pause_merge".into(),
            Request::ResumeMerge => "resume_merge".into(),
            Request::Stop => "stop".into(),
            Request::Detach => "detach".into(),
        }
    }

    /// The request a line says, or an error for a word this daemon does not
    /// know: a message that is passed over in silence would leave the caller
    /// waiting for an answer it will never get.
    pub fn parse(text: &str) -> Result<Request> {
        let text = text.trim_end_matches('\0');
        let (word, rest) = match text.split_once(',') {
            Some((word, rest)) => (word, Some(rest)),
            None => (text, None),
        };
        let needing = |what: &str| -> Result<String> {
            match rest {
                Some(rest) if !rest.is_empty() => Ok(rest.to_string()),
                _ => bail!("{word} names no {what}"),
            }
        };
        Ok(match word {
            "query" => Request::Query,
            "init" => {
                let named = needing("device")?;
                let mut fields = named.split(',');
                let (misc, cow, backing) = match (fields.next(), fields.next(), fields.next()) {
                    (Some(misc), Some(cow), Some(backing)) => (misc, cow, backing),
                    _ => bail!("init takes a device, a copy-on-write device and what it reads through"),
                };
                let merge_target = fields.next().map(str::to_string);
                if fields.next().is_some() || [misc, cow, backing].iter().any(|field| field.is_empty()) {
                    bail!("init takes a device, a copy-on-write device and what it reads through, and the merge target if there is one");
                }
                Request::Init {
                    misc: misc.to_string(),
                    cow: cow.to_string(),
                    backing: backing.to_string(),
                    merge_target,
                }
            }
            "start" => Request::Start { misc: needing("device")? },
            "delete" => Request::Delete { device: needing("device")? },
            "initiate_merge" => Request::InitiateMerge { misc: needing("device")? },
            "merge_percent" => Request::MergePercent,
            "getstatus" => Request::Status { misc: needing("device")? },
            "update-verify" => Request::VerifyUpdate,
            "pause_merge" => Request::PauseMerge,
            "resume_merge" => Request::ResumeMerge,
            "stop" => Request::Stop,
            "detach" => Request::Detach,
            other => bail!("{other:?} is not a message this daemon knows"),
        })
    }
}

/// What the daemon answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// `query`'s answer: serving, or on its way out.
    Active,
    Passive,
    /// A plain `success`.
    Done,
    /// `init`'s answer: how many sectors the device it made has.
    Sectors(u64),
    /// `merge_percent`'s answer.
    Percent(f64),
    /// `getstatus`'s answer, in the daemon's own words.
    Status(String),
    /// An answer this reader does not recognise, kept as it is.
    Other(String),
}

impl Reply {
    /// The reply these bytes say.
    pub fn parse(text: &str) -> Reply {
        let text = text.trim_end_matches('\0');
        if text == "success" {
            return Reply::Done;
        }
        if let Some(rest) = text.strip_prefix("success,") {
            if let Ok(sectors) = rest.trim().parse::<u64>() {
                return Reply::Sectors(sectors);
            }
        }
        match text {
            "active" => Reply::Active,
            "passive" => Reply::Passive,
            _ => match text.trim().parse::<f64>() {
                Ok(percent) if !text.trim().is_empty() => Reply::Percent(percent),
                _ => {
                    // `getstatus` answers in states such as these; anything
                    // else is kept rather than thrown away.
                    const STATES: [&str; 5] =
                        ["none", "snapshot", "merging", "snapshot-merge", "snapshot-merge-failed"];
                    if STATES.contains(&text) {
                        Reply::Status(text.to_string())
                    } else {
                        Reply::Other(text.to_string())
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header the kernel sends for 8 sectors at sector 2048, as the
    /// little-endian packing lays it.
    #[test]
    fn a_request_is_forty_bytes_and_reads_back_as_it_was_written() {
        let header = Header { seq: 7, kind: REQ_MAP_READ, flags: 0, sector: 2048, len: 8 };
        let bytes = header.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[0..8], &7u64.to_le_bytes(), "seq first");
        assert_eq!(&bytes[8..16], &0u64.to_le_bytes(), "then the type");
        assert_eq!(&bytes[24..32], &2048u64.to_le_bytes(), "then the sector");
        assert_eq!(&bytes[32..40], &8u64.to_le_bytes(), "and the length last");
        assert_eq!(Header::decode(&bytes).expect("its own bytes"), header);
    }

    #[test]
    fn a_header_cut_short_is_refused_rather_than_read_past_its_end() {
        let error = Header::decode(&[0u8; HEADER_LEN - 1]).unwrap_err().to_string();
        assert!(error.contains("40 bytes"), "{error}");
    }

    #[test]
    fn the_answer_is_the_question_with_its_type_changed_and_the_blocks_after_it() {
        let asked = Header { seq: 3, kind: REQ_MAP_READ, flags: 0x99, sector: 512, len: 4 };
        let answered = asked.answer(Answered::Success, 4);
        assert_eq!(answered.seq, asked.seq, "the kernel is answered in its own sequence");
        assert_eq!(answered.kind, RESP_SUCCESS);
        assert_eq!((answered.sector, answered.len), (asked.sector, asked.len), "the same blocks");
        assert_eq!(answered.flags, 0, "and no flags of its own");
        assert!(asked.fits_in(HEADER_LEN + 4 * SECTOR as usize), "four sectors fit after the header");
        assert!(!asked.fits_in(HEADER_LEN + 3 * SECTOR as usize), "three do not");
    }

    /// A `0` is a read when the kernel sends it and a success when the daemon
    /// answers, and a `1` is a write or an error: the same number means two
    /// things, so it is read by the direction the header went and not alone.
    #[test]
    fn the_same_number_is_a_different_thing_each_way() {
        assert_eq!((REQ_MAP_READ, RESP_SUCCESS), (0, 0));
        assert_eq!((REQ_MAP_WRITE, RESP_ERROR), (1, 1));
        let read = Header { seq: 1, kind: REQ_MAP_READ, flags: 0, sector: 0, len: 1 };
        assert_eq!(read.asked(), Some(Asked::Read), "from the kernel, 0 asks to read");
        assert_eq!(read.answering(), Some(Answered::Success), "from the daemon, 0 says it went well");
        let write = Header { seq: 2, kind: REQ_MAP_WRITE, flags: 0, sector: 0, len: 1 };
        assert_eq!(write.asked(), Some(Asked::Write));
        assert_eq!(write.answering(), Some(Answered::Error));
        let unsupported = Header { seq: 3, kind: RESP_UNSUPPORTED, flags: 0, sector: 0, len: 0 };
        assert_eq!(unsupported.asked(), None, "2 is no request at all");
        assert_eq!(unsupported.answering(), Some(Answered::Unsupported));
    }

    #[test]
    fn every_message_is_the_words_the_daemon_reads() {
        let cases = [
            (Request::Query, "query"),
            (Request::MergePercent, "merge_percent"),
            (Request::VerifyUpdate, "update-verify"),
            (Request::PauseMerge, "pause_merge"),
            (Request::ResumeMerge, "resume_merge"),
            (Request::Stop, "stop"),
            (Request::Detach, "detach"),
            (Request::Start { misc: "user_snapshot-system_a".into() }, "start,user_snapshot-system_a"),
            (Request::Delete { device: "system_a-cow".into() }, "delete,system_a-cow"),
            (Request::InitiateMerge { misc: "system_a".into() }, "initiate_merge,system_a"),
            (Request::Status { misc: "system_a".into() }, "getstatus,system_a"),
        ];
        for (request, text) in cases {
            assert_eq!(request.text(), text);
            assert_eq!(Request::parse(text).expect(text), request, "{text} reads back");
        }
    }

    #[test]
    fn an_init_names_three_devices_and_the_merge_target_only_where_there_is_one() {
        let plain = Request::Init { misc: "system_a".into(), cow: "system_a-cow".into(), backing: "/dev/block/dm-3".into(), merge_target: None };
        assert_eq!(plain.text(), "init,system_a,system_a-cow,/dev/block/dm-3");
        assert_eq!(Request::parse(&plain.text()).expect("reads back"), plain);
        let merged = Request::Init {
            misc: "system_a".into(),
            cow: "system_a-cow".into(),
            backing: "/dev/block/dm-3".into(),
            merge_target: Some("/data/snapshots/system_a".into()),
        };
        assert_eq!(merged.text(), "init,system_a,system_a-cow,/dev/block/dm-3,/data/snapshots/system_a");
        assert_eq!(Request::parse(&merged.text()).expect("reads back"), merged);
    }

    #[test]
    fn a_message_without_its_device_or_unknown_to_the_daemon_is_refused() {
        for bad in ["start", "start,", "init", "init,only_one", "getstatus", "initiate_merge"] {
            assert!(Request::parse(bad).is_err(), "{bad:?} names nothing to act on");
        }
        let error = Request::parse("fly,away").unwrap_err().to_string();
        assert!(error.contains("is not a message"), "{error}");
    }

    #[test]
    fn the_answers_are_read_as_the_daemon_writes_them() {
        assert_eq!(Reply::parse("active"), Reply::Active);
        assert_eq!(Reply::parse("passive"), Reply::Passive);
        assert_eq!(Reply::parse("success"), Reply::Done);
        assert_eq!(Reply::parse("success,262144"), Reply::Sectors(262144));
        assert_eq!(Reply::parse("42.5"), Reply::Percent(42.5));
        assert_eq!(Reply::parse("0"), Reply::Percent(0.0));
        assert_eq!(Reply::parse("snapshot-merge-failed"), Reply::Status("snapshot-merge-failed".into()));
        assert_eq!(Reply::parse("something else"), Reply::Other("something else".into()));
        assert_eq!(Reply::parse(""), Reply::Other(String::new()));
    }

    #[test]
    fn a_reply_that_trails_a_nul_is_read_as_the_word_before_it() {
        // The daemon writes its answer with the string's own terminator.
        assert_eq!(Reply::parse("active\0"), Reply::Active);
        assert_eq!(Reply::parse("success\0"), Reply::Done);
        assert_eq!(Request::parse("query\0").expect("reads"), Request::Query);
    }
}
