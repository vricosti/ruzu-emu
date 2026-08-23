// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-FileCopyrightText: Copyright 1996 Arthur David Olson
// SPDX-License-Identifier: GPL-2.0-or-later AND BSD-2-Clause

//! Switch TZif binary parser and timezone conversion functions.
//!
//! Port of zuyu/externals/tz/tz/tz.h and tz.cpp (Arthur David Olson's
//! IANA timezone library). Parses the Switch timezone archive's TZif-derived
//! files and provides
//! localtime_rz / mktime_tzname equivalent conversions.
//!
//! Used by the PSC time service to convert between POSIX timestamps and
//! calendar time with timezone support.

// Constants matching upstream tz.cpp
pub(crate) const TZ_MAX_TIMES: usize = 1000;
pub(crate) const TZ_MAX_TYPES: usize = 128;
pub(crate) const TZ_MAX_CHARS: usize = 50;
const TZ_MAX_LEAPS: usize = 50;
const TZNAME_MAXIMUM: usize = 255;
const CHARS_EXTRA: usize = 3;

const SECSPERMIN: i64 = 60;
const MINSPERHOUR: i64 = 60;
const SECSPERHOUR: i64 = SECSPERMIN * MINSPERHOUR;
const HOURSPERDAY: i64 = 24;
const SECSPERDAY: i64 = SECSPERHOUR * HOURSPERDAY;
const DAYSPERWEEK: i64 = 7;
const DAYSPERNYEAR: i64 = 365;
const DAYSPERLYEAR: i64 = 366;
const MONSPERYEAR: usize = 12;
const YEARSPERREPEAT: i64 = 400;
const DAYSPERREPEAT: i64 = 400 * 365 + 100 - 4 + 1;
const SECSPERREPEAT: i64 = DAYSPERREPEAT * SECSPERDAY;
const AVGSECSPERYEAR: i64 = SECSPERREPEAT / YEARSPERREPEAT;

pub const TM_YEAR_BASE: i64 = 1900;
const EPOCH_YEAR: i64 = 1970;
// Monday = 1
const TM_WDAY_BASE: i64 = 1; // TM_MONDAY

const TIME_T_MAX: i64 = i64::MAX;
const TIME_T_MIN: i64 = i64::MIN;

const UNSPEC: &[u8] = b"-00\0";

const TZIF_HEADER_SIZE: usize = 0x2C;

static MON_LENGTHS: [[i32; MONSPERYEAR]; 2] = [
    [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
];

static YEAR_LENGTHS: [i64; 2] = [DAYSPERNYEAR, DAYSPERLYEAR];

pub fn is_leap(y: i64) -> bool {
    (y % 4 == 0) && (y % 100 != 0 || y % 400 == 0)
}

fn is_leap_idx(y: i64) -> usize {
    if is_leap(y) {
        1
    } else {
        0
    }
}

fn leaps_thru_end_of_nonneg(y: i64) -> i64 {
    y / 4 - y / 100 + y / 400
}

fn leaps_thru_end_of(y: i64) -> i64 {
    if y < 0 {
        -1 - leaps_thru_end_of_nonneg(-1 - y)
    } else {
        leaps_thru_end_of_nonneg(y)
    }
}

/// Decode a big-endian 4-byte signed integer (matching upstream `detzcode`).
pub fn detzcode(data: &[u8]) -> i32 {
    let mut result = (data[0] & 0x7f) as i32;
    for i in 1..4 {
        result = (result << 8) | (data[i] as i32);
    }
    if data[0] & 0x80 != 0 {
        // Two's complement: result += i32::MIN
        result = result.wrapping_add(i32::MIN);
    }
    result
}

/// Decode a big-endian 8-byte signed integer (matching upstream `detzcode64`).
pub fn detzcode64(data: &[u8]) -> i64 {
    let mut result = (data[0] & 0x7f) as i64;
    for i in 1..8 {
        result = (result << 8) | (data[i] as i64);
    }
    if data[0] & 0x80 != 0 {
        result = result.wrapping_add(i64::MIN);
    }
    result
}

/// Type info for a timezone transition (matching upstream `ttinfo`).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct TtInfo {
    pub tt_utoff: i32,
    pub tt_isdst: u8,
    pub _padding0: [u8; 3],
    pub tt_desigidx: i32,
    pub tt_ttisstd: u8,
    pub tt_ttisut: u8,
    pub _padding1: [u8; 2],
}
const _: () = assert!(core::mem::size_of::<TtInfo>() == 0x10);

/// Parsed TZif timezone rule (matching upstream `Tz::Rule`).
///
/// Contains transition times, type indices, type info records, and
/// abbreviation strings parsed from a TZif binary.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TzRule {
    pub timecnt: i32,
    pub typecnt: i32,
    pub charcnt: i32,
    pub goback: u8,
    pub goahead: u8,
    pub _padding0: [u8; 2],
    pub ats: [i64; TZ_MAX_TIMES],
    pub types: [u8; TZ_MAX_TIMES],
    pub ttis: [TtInfo; TZ_MAX_TYPES],
    pub chars: [u8; 2 * (TZNAME_MAXIMUM + 1)],
    pub default_type: i32,
    pub _padding1: [u8; 0x12C4],
}
const _: () = assert!(core::mem::size_of::<TzRule>() == 0x4000);

impl Default for TzRule {
    fn default() -> Self {
        // Matches C++ value-initialization (`Tz::Rule{}`). Runtime UTC rules
        // are parsed from the timezone archive rather than invented here.
        Self::zeroed()
    }
}

impl TzRule {
    fn zeroed() -> Self {
        Self {
            timecnt: 0,
            typecnt: 0,
            charcnt: 0,
            goback: 0,
            goahead: 0,
            _padding0: [0; 2],
            ats: [0; TZ_MAX_TIMES],
            types: [0; TZ_MAX_TIMES],
            ttis: [TtInfo::default(); TZ_MAX_TYPES],
            chars: [0; 2 * (TZNAME_MAXIMUM + 1)],
            default_type: 0,
            _padding1: [0; 0x12C4],
        }
    }

    /// Decode the fixed-size IPC payload without imposing alignment on the
    /// guest-provided byte buffer. Every field uses an all-bit-pattern-valid
    /// integer representation.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..core::mem::size_of::<Self>())?;
        Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<Self>()) })
    }

    /// Reconstruct an upstream `InLargeData<Rule>` argument: value-initialize
    /// the complete object, then copy as much of the guest buffer as exists.
    pub fn from_prefix_bytes(bytes: &[u8]) -> Self {
        let mut rule = Self::default();
        let copy_len = bytes.len().min(core::mem::size_of::<Self>());
        let output = unsafe {
            core::slice::from_raw_parts_mut(
                (&mut rule as *mut Self).cast::<u8>(),
                core::mem::size_of::<Self>(),
            )
        };
        output[..copy_len].copy_from_slice(&bytes[..copy_len]);
        rule
    }

    /// Return the complete deterministic 0x4000-byte IPC representation.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                (self as *const Self).cast::<u8>(),
                core::mem::size_of::<Self>(),
            )
        }
    }

    /// Check if a type abbreviation is the unspecified marker "-00".
    fn ttunspecified(&self, i: usize) -> bool {
        let idx = self.ttis[i].tt_desigidx as usize;
        if idx + UNSPEC.len() > self.chars.len() {
            return false;
        }
        &self.chars[idx..idx + UNSPEC.len()] == UNSPEC
    }

    /// Check if two type indices are equivalent (matching upstream `typesequiv`).
    fn typesequiv(&self, a: usize, b: usize) -> bool {
        if a >= self.typecnt as usize || b >= self.typecnt as usize {
            return false;
        }
        let ap = &self.ttis[a];
        let bp = &self.ttis[b];
        if ap.tt_utoff != bp.tt_utoff || ap.tt_isdst != bp.tt_isdst {
            return false;
        }
        // Compare abbreviation strings
        let a_idx = ap.tt_desigidx as usize;
        let b_idx = bp.tt_desigidx as usize;
        let a_abbr = self.get_abbrev(a_idx);
        let b_abbr = self.get_abbrev(b_idx);
        a_abbr == b_abbr
    }

    /// Get a null-terminated abbreviation string starting at the given index.
    fn get_abbrev(&self, idx: usize) -> &[u8] {
        if idx >= self.chars.len() {
            return b"";
        }
        let end = self.chars[idx..]
            .iter()
            .position(|&c| c == 0)
            .map(|p| idx + p)
            .unwrap_or(self.chars.len());
        &self.chars[idx..end]
    }

    /// Parse a TZif binary into a TzRule (matching upstream `tzloadbody`).
    ///
    /// The Switch timezone archive stores a single data block with 8-byte
    /// timestamps, even though its header carries the `TZif2` version marker.
    /// This deliberately matches Eden's `tzloadbody`, which reads that first
    /// block directly with `stored = 8` instead of applying the standard TZif
    /// v2 two-block layout.
    pub fn parse(binary: &[u8]) -> Option<TzRule> {
        if binary.len() < TZIF_HEADER_SIZE {
            return None;
        }

        // Validate magic
        if &binary[0..4] != b"TZif" {
            return None;
        }

        Self::parse_data_block(binary, 8)
    }

    /// Parse a TZif data block (header + data) with the given timestamp size
    /// (4 for V1, 8 for V2/V3). Matches upstream `tzloadbody`.
    fn parse_data_block(header_and_data: &[u8], stored: usize) -> Option<TzRule> {
        if header_and_data.len() < TZIF_HEADER_SIZE {
            return None;
        }

        let ttisutcnt = detzcode(&header_and_data[20..24]);
        let ttisstdcnt = detzcode(&header_and_data[24..28]);
        let leapcnt = detzcode(&header_and_data[28..32]);
        let timecnt_orig = detzcode(&header_and_data[32..36]);
        let typecnt = detzcode(&header_and_data[36..40]);
        let charcnt = detzcode(&header_and_data[40..44]);

        // Validate counts
        if !(0 <= leapcnt
            && (leapcnt as usize) < TZ_MAX_LEAPS
            && 0 <= typecnt
            && (typecnt as usize) < TZ_MAX_TYPES
            && 0 <= timecnt_orig
            && (timecnt_orig as usize) < TZ_MAX_TIMES
            && 0 <= charcnt
            && (charcnt as usize) < TZ_MAX_CHARS
            && 0 <= ttisstdcnt
            && (ttisstdcnt as usize) < TZ_MAX_TYPES
            && 0 <= ttisutcnt
            && (ttisutcnt as usize) < TZ_MAX_TYPES)
        {
            return None;
        }

        let datablock_size = (timecnt_orig as i64 * stored as i64)
            + timecnt_orig as i64
            + typecnt as i64 * 6
            + charcnt as i64
            + leapcnt as i64 * (stored as i64 + 4)
            + ttisstdcnt as i64
            + ttisutcnt as i64;

        if (header_and_data.len() as i64) < TZIF_HEADER_SIZE as i64 + datablock_size {
            return None;
        }

        if !((ttisstdcnt == typecnt || ttisstdcnt == 0) && (ttisutcnt == typecnt || ttisutcnt == 0))
        {
            return None;
        }

        let mut p = TZIF_HEADER_SIZE;

        let mut sp = TzRule::zeroed();
        sp.timecnt = timecnt_orig;
        sp.typecnt = typecnt;
        sp.charcnt = charcnt;

        // Read transitions, discarding those out of time_t range.
        // Matching upstream logic exactly.
        let mut timecnt: usize = 0;

        // First pass: read transition times
        // sp.types[i] is temporarily used as a flag: 1 if in range, 0 if not
        for i in 0..sp.timecnt as usize {
            let at = if stored == 4 {
                detzcode(&header_and_data[p..p + 4]) as i64
            } else {
                detzcode64(&header_and_data[p..p + 8])
            };
            sp.types[i] = if at <= TIME_T_MAX { 1 } else { 0 };
            if sp.types[i] != 0 {
                let attime = if at < TIME_T_MIN { TIME_T_MIN } else { at };
                if timecnt > 0 && attime <= sp.ats[timecnt - 1] {
                    if attime < sp.ats[timecnt - 1] {
                        return None; // EINVAL
                    }
                    sp.types[i - 1] = 0;
                    timecnt -= 1;
                }
                sp.ats[timecnt] = attime;
                timecnt += 1;
            }
            p += stored;
        }

        // Second pass: read type indices
        let mut timecnt2: usize = 0;
        for i in 0..sp.timecnt as usize {
            let typ = header_and_data[p];
            p += 1;
            if typ as i32 >= sp.typecnt {
                return None; // EINVAL
            }
            if sp.types[i] != 0 {
                sp.types[timecnt2] = typ;
                timecnt2 += 1;
            }
        }
        sp.timecnt = timecnt as i32;

        // Read ttinfo records
        for i in 0..sp.typecnt as usize {
            sp.ttis[i].tt_utoff = detzcode(&header_and_data[p..p + 4]);
            p += 4;
            let isdst = header_and_data[p];
            p += 1;
            if isdst >= 2 {
                return None; // EINVAL
            }
            sp.ttis[i].tt_isdst = isdst;
            let desigidx = header_and_data[p];
            p += 1;
            if desigidx as i32 >= sp.charcnt {
                return None; // EINVAL
            }
            sp.ttis[i].tt_desigidx = desigidx as i32;
        }

        // Read abbreviation characters
        for i in 0..sp.charcnt as usize {
            sp.chars[i] = header_and_data[p];
            p += 1;
        }
        // Ensure null-terminated with CHARS_EXTRA
        let ci = sp.charcnt as usize;
        for j in 0..CHARS_EXTRA {
            if ci + j < sp.chars.len() {
                sp.chars[ci + j] = 0;
            }
        }

        // Read ttisstd flags
        for i in 0..sp.typecnt as usize {
            if ttisstdcnt == 0 {
                sp.ttis[i].tt_ttisstd = 0;
            } else {
                let val = header_and_data[p];
                p += 1;
                if val > 1 {
                    return None; // EINVAL
                }
                sp.ttis[i].tt_ttisstd = val;
            }
        }

        // Read ttisut flags
        for i in 0..sp.typecnt as usize {
            if ttisutcnt == 0 {
                sp.ttis[i].tt_ttisut = 0;
            } else {
                let val = header_and_data[p];
                p += 1;
                if val > 1 {
                    return None; // EINVAL
                }
                sp.ttis[i].tt_ttisut = val;
            }
        }

        // Handle POSIX TZ string after V2 data (the trailing newline-delimited string).
        // We compute nread as the remaining data after parsing, matching upstream.
        let total_len = header_and_data.len();
        let nread = if total_len > p { total_len - p } else { 0 };
        if nread > 256 {
            return None;
        }

        if nread > 2
            && header_and_data[total_len - nread] == b'\n'
            && header_and_data[total_len - 1] == b'\n'
            && sp.typecnt + 2 <= TZ_MAX_TYPES as i32
        {
            // We have a POSIX TZ string. Parse it to extend transitions.
            let tz_str = &header_and_data[total_len - nread + 1..total_len - 1];
            if let Some(ts) = parse_posix_tz(tz_str) {
                if ts.typecnt == 2 {
                    // Attempt to reuse existing abbreviations
                    let mut gotabbr = 0;
                    let mut charcnt = sp.charcnt as usize;
                    let mut ts_desig = [0i32; TZ_MAX_TYPES];
                    for i in 0..ts.typecnt as usize {
                        ts_desig[i] = ts.ttis[i].tt_desigidx;
                        let tsabbr = ts.get_abbrev(ts.ttis[i].tt_desigidx as usize);
                        let mut found = false;
                        for j in 0..charcnt {
                            let sp_abbr = sp.get_abbrev(j);
                            if sp_abbr == tsabbr {
                                ts_desig[i] = j as i32;
                                gotabbr += 1;
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            let tsabbrlen = tsabbr.len();
                            if charcnt + tsabbrlen < TZ_MAX_CHARS {
                                for (k, &b) in tsabbr.iter().enumerate() {
                                    if charcnt + k < sp.chars.len() {
                                        sp.chars[charcnt + k] = b;
                                    }
                                }
                                if charcnt + tsabbrlen < sp.chars.len() {
                                    sp.chars[charcnt + tsabbrlen] = 0;
                                }
                                ts_desig[i] = charcnt as i32;
                                charcnt = charcnt + tsabbrlen + 1;
                                gotabbr += 1;
                            }
                        }
                    }
                    if gotabbr == ts.typecnt {
                        sp.charcnt = charcnt as i32;

                        // Ignore trailing no-op transitions
                        while sp.timecnt > 1
                            && sp.types[sp.timecnt as usize - 1]
                                == sp.types[sp.timecnt as usize - 2]
                        {
                            sp.timecnt -= 1;
                        }

                        for i in 0..ts.timecnt as usize {
                            if (sp.timecnt as usize) >= TZ_MAX_TIMES {
                                break;
                            }
                            let t = ts.ats[i];
                            if sp.timecnt > 0 && t <= sp.ats[sp.timecnt as usize - 1] {
                                continue;
                            }
                            sp.ats[sp.timecnt as usize] = t;
                            sp.types[sp.timecnt as usize] =
                                (sp.typecnt as u8).wrapping_add(ts.types[i]);
                            sp.timecnt += 1;
                        }
                        for i in 0..ts.typecnt as usize {
                            let mut tti = ts.ttis[i];
                            tti.tt_desigidx = ts_desig[i];
                            sp.ttis[sp.typecnt as usize] = tti;
                            sp.typecnt += 1;
                        }
                    }
                }
            }
        }

        if sp.typecnt == 0 {
            return None;
        }

        // Determine goback / goahead (matching upstream)
        if sp.timecnt > 1 {
            if sp.ats[0] <= TIME_T_MAX - SECSPERREPEAT {
                let repeatat = sp.ats[0] + SECSPERREPEAT;
                let repeattype = sp.types[0] as usize;
                for i in 1..sp.timecnt as usize {
                    if sp.ats[i] == repeatat && sp.typesequiv(sp.types[i] as usize, repeattype) {
                        sp.goback = 1;
                        break;
                    }
                }
            }
            if TIME_T_MIN + SECSPERREPEAT <= sp.ats[sp.timecnt as usize - 1] {
                let repeatat = sp.ats[sp.timecnt as usize - 1] - SECSPERREPEAT;
                let repeattype = sp.types[sp.timecnt as usize - 1] as usize;
                for i in (0..sp.timecnt as usize - 1).rev() {
                    if sp.ats[i] == repeatat && sp.typesequiv(sp.types[i] as usize, repeattype) {
                        sp.goahead = 1;
                        break;
                    }
                }
            }
        }

        // Infer defaulttype (matching upstream heuristics)
        let mut dt: i32;

        // If type 0 is used in transitions and is not unspecified, set dt = -1
        // to indicate we need further search. Otherwise use type 0.
        let mut type0_used = false;
        for i in 0..sp.timecnt as usize {
            if sp.types[i] == 0 {
                type0_used = true;
                break;
            }
        }
        if type0_used && !sp.ttunspecified(0) {
            dt = -1;
        } else {
            dt = 0;
        }

        // If dt < 0 and there are transitions and the first transition is to DST,
        // find the standard type less than and closest to the type of the first transition.
        if dt < 0 && sp.timecnt > 0 && sp.ttis[sp.types[0] as usize].tt_isdst != 0 {
            dt = sp.types[0] as i32;
            while dt > 0 {
                dt -= 1;
                if sp.ttis[dt as usize].tt_isdst == 0 {
                    break;
                }
            }
            if dt >= 0 && sp.ttis[dt as usize].tt_isdst != 0 {
                dt = -1; // all were DST
            }
        }

        // If no result yet, find the first standard type. If none, use 0.
        if dt < 0 {
            dt = 0;
            while sp.ttis[dt as usize].tt_isdst != 0 {
                dt += 1;
                if dt >= sp.typecnt {
                    dt = 0;
                    break;
                }
            }
        }

        sp.default_type = dt;

        Some(sp)
    }
}

/// Internal calendar time representation matching upstream `CalendarTimeInternal`.
#[derive(Debug, Clone, Default)]
pub struct CalendarTimeInternal {
    pub tm_sec: i32,
    pub tm_min: i32,
    pub tm_hour: i32,
    pub tm_mday: i32,
    pub tm_mon: i32,
    pub tm_year: i32,
    pub tm_wday: i32,
    pub tm_yday: i32,
    pub tm_isdst: i32,
    pub tm_zone: [u8; 16],
    pub tm_utoff: i32,
    pub time_index: i32,
}

/// Convert a timestamp to broken-down time components (matching upstream `timesub`).
fn timesub(timep: i64, offset: i64, _sp: &TzRule) -> Option<CalendarTimeInternal> {
    let mut tmp = CalendarTimeInternal::default();

    let tdays = timep / SECSPERDAY;
    let rem_init = timep % SECSPERDAY;
    let rem_with_offset = rem_init + offset % SECSPERDAY + 3 * SECSPERDAY;
    let dayoff = offset / SECSPERDAY + rem_with_offset / SECSPERDAY - 3;
    let mut rem = rem_with_offset % SECSPERDAY;

    // Calculate year, matching upstream's overflow-safe algorithm
    let dayrem_t = tdays % DAYSPERREPEAT;
    let dayrem = dayrem_t + dayoff % DAYSPERREPEAT;
    let mut y: i64 = EPOCH_YEAR - YEARSPERREPEAT
        + ((1i64
            .wrapping_add(dayoff / DAYSPERREPEAT)
            .wrapping_add(dayrem / DAYSPERREPEAT)
            .wrapping_sub(if dayrem % DAYSPERREPEAT < 0 { 1 } else { 0 })
            .wrapping_add(tdays / DAYSPERREPEAT))
            * YEARSPERREPEAT);

    let mut idays = tdays % DAYSPERREPEAT;
    idays += dayoff % DAYSPERREPEAT + 2 * DAYSPERREPEAT;
    idays %= DAYSPERREPEAT;

    // Increase y and decrease idays until idays is in range for y
    while YEAR_LENGTHS[is_leap_idx(y)] <= idays {
        let tdelta = idays / DAYSPERLYEAR;
        let ydelta = if tdelta == 0 { 1 } else { tdelta };
        let newy = y + ydelta;
        let leapdays = leaps_thru_end_of(newy - 1) - leaps_thru_end_of(y - 1);
        idays -= ydelta * DAYSPERNYEAR;
        idays -= leapdays;
        y = newy;
    }

    // Check year fits in i32
    if y - TM_YEAR_BASE < i32::MIN as i64 || y - TM_YEAR_BASE > i32::MAX as i64 {
        return None;
    }
    tmp.tm_year = (y - TM_YEAR_BASE) as i32;

    tmp.tm_yday = idays as i32;

    // Day of week
    tmp.tm_wday = (TM_WDAY_BASE
        + ((tmp.tm_year as i64 % DAYSPERWEEK) * (DAYSPERNYEAR % DAYSPERWEEK))
        + leaps_thru_end_of(y - 1)
        - leaps_thru_end_of(TM_YEAR_BASE - 1)
        + idays) as i32;
    tmp.tm_wday = tmp.tm_wday.rem_euclid(DAYSPERWEEK as i32);

    tmp.tm_hour = (rem / SECSPERHOUR) as i32;
    rem %= SECSPERHOUR;
    tmp.tm_min = (rem / SECSPERMIN) as i32;
    tmp.tm_sec = (rem % SECSPERMIN) as i32;

    let ip = &MON_LENGTHS[is_leap_idx(y)];
    let mut idays_remaining = idays;
    tmp.tm_mon = 0;
    while tmp.tm_mon < MONSPERYEAR as i32 && idays_remaining >= ip[tmp.tm_mon as usize] as i64 {
        idays_remaining -= ip[tmp.tm_mon as usize] as i64;
        tmp.tm_mon += 1;
    }
    tmp.tm_mday = idays_remaining as i32 + 1;
    tmp.tm_isdst = 0;

    Some(tmp)
}

/// Matching upstream `localsub`: find the right transition for a given timestamp,
/// apply the offset, and compute the broken-down time.
pub fn localsub(sp: &TzRule, t: i64) -> Option<CalendarTimeInternal> {
    // Handle goback/goahead (cyclic timezone rules)
    if sp.timecnt > 0 {
        if (sp.goback != 0 && t < sp.ats[0])
            || (sp.goahead != 0 && t > sp.ats[sp.timecnt as usize - 1])
        {
            let seconds_raw = if t < sp.ats[0] {
                sp.ats[0] - t
            } else {
                t - sp.ats[sp.timecnt as usize - 1]
            };
            let seconds = seconds_raw - 1;

            let years = seconds / SECSPERREPEAT * YEARSPERREPEAT;
            let secs_adj = years * AVGSECSPERYEAR;
            let years_total = years + YEARSPERREPEAT;

            let newt = if t < sp.ats[0] {
                t + secs_adj + SECSPERREPEAT
            } else {
                t - secs_adj - SECSPERREPEAT
            };

            if sp.timecnt > 0 && (newt < sp.ats[0] || newt > sp.ats[sp.timecnt as usize - 1]) {
                return None;
            }

            let mut result = localsub(sp, newt)?;

            let newy = if t < sp.ats[0] {
                result.tm_year as i64 - years_total
            } else {
                result.tm_year as i64 + years_total
            };

            if newy < i32::MIN as i64 || newy > i32::MAX as i64 {
                return None;
            }
            result.tm_year = newy as i32;
            return Some(result);
        }
    }

    // Find the transition type
    let i = if sp.timecnt == 0 || t < sp.ats[0] {
        sp.default_type as usize
    } else {
        // Binary search: find largest index where ats[index] <= t
        let mut lo: usize = 1;
        let mut hi = sp.timecnt as usize;
        while lo < hi {
            let mid = (lo + hi) >> 1;
            if t < sp.ats[mid] {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        sp.types[lo - 1] as usize
    };

    if i >= sp.ttis.len() {
        return None;
    }

    let ttisp = &sp.ttis[i];

    let mut result = timesub(t, ttisp.tt_utoff as i64, sp)?;
    result.tm_isdst = i32::from(ttisp.tt_isdst != 0);

    // Copy abbreviation
    let desig_idx = ttisp.tt_desigidx as usize;
    if desig_idx < sp.chars.len() {
        let abbr = sp.get_abbrev(desig_idx);
        let copy_len = abbr.len().min(result.tm_zone.len() - 1);
        result.tm_zone[..copy_len].copy_from_slice(&abbr[..copy_len]);
        result.tm_zone[copy_len] = 0;
    }

    result.tm_utoff = ttisp.tt_utoff;
    result.time_index = i as i32;
    Some(result)
}

/// Compare two CalendarTimeInternal values (matching upstream `tmcomp`).
fn tmcomp(a: &CalendarTimeInternal, b: &CalendarTimeInternal) -> i32 {
    if a.tm_year != b.tm_year {
        return if a.tm_year < b.tm_year { -1 } else { 1 };
    }
    let mut result = a.tm_mon - b.tm_mon;
    if result == 0 {
        result = a.tm_mday - b.tm_mday;
        if result == 0 {
            result = a.tm_hour - b.tm_hour;
            if result == 0 {
                result = a.tm_min - b.tm_min;
                if result == 0 {
                    result = a.tm_sec - b.tm_sec;
                }
            }
        }
    }
    result
}

fn normalize_overflow(tens: &mut i32, units: &mut i32, base: i32) -> bool {
    let tensdelta = if *units >= 0 {
        *units / base
    } else {
        -1 - (-1 - *units) / base
    };
    *units -= tensdelta * base;
    // Check overflow of *tens + tensdelta
    if ((*tens >= 0) && (tensdelta > i32::MAX - *tens))
        || ((*tens < 0) && (tensdelta < i32::MIN - *tens))
    {
        return true;
    }
    *tens += tensdelta;
    false
}

fn normalize_overflow32(tens: &mut i64, units: &mut i32, base: i32) -> bool {
    let tensdelta = if *units >= 0 {
        *units / base
    } else {
        -1 - (-1 - *units) / base
    };
    *units -= tensdelta * base;
    let td = tensdelta as i64;
    if ((*tens >= 0) && (td > i32::MAX as i64 - *tens))
        || ((*tens < 0) && (td < i32::MIN as i64 - *tens))
    {
        return true;
    }
    *tens += td;
    false
}

fn increment_overflow32(lp: &mut i64, m: i32) -> bool {
    let l = *lp;
    let m64 = m as i64;
    if (l >= 0 && m64 > i32::MAX as i64 - l) || (l < 0 && m64 < i32::MIN as i64 - l) {
        return true;
    }
    *lp += m64;
    false
}

/// Matching upstream `time2sub`: binary search for a time_t that produces the
/// requested broken-down time.
fn time2sub(
    sp: &TzRule,
    tmp_in: &CalendarTimeInternal,
    do_norm_secs: bool,
) -> Result<(i64, CalendarTimeInternal), u32> {
    let mut yourtm = tmp_in.clone();

    if do_norm_secs {
        if normalize_overflow(&mut yourtm.tm_min, &mut yourtm.tm_sec, SECSPERMIN as i32) {
            return Err(1);
        }
    }
    if normalize_overflow(&mut yourtm.tm_hour, &mut yourtm.tm_min, MINSPERHOUR as i32) {
        return Err(1);
    }
    if normalize_overflow(&mut yourtm.tm_mday, &mut yourtm.tm_hour, HOURSPERDAY as i32) {
        return Err(1);
    }
    let mut y = yourtm.tm_year as i64;
    if normalize_overflow32(&mut y, &mut yourtm.tm_mon, MONSPERYEAR as i32) {
        return Err(1);
    }
    if increment_overflow32(&mut y, TM_YEAR_BASE as i32) {
        return Err(1);
    }
    while yourtm.tm_mday <= 0 {
        if increment_overflow32(&mut y, -1) {
            return Err(1);
        }
        let li = y + if 1 < yourtm.tm_mon { 1 } else { 0 };
        yourtm.tm_mday += YEAR_LENGTHS[is_leap_idx(li)] as i32;
    }
    while yourtm.tm_mday > DAYSPERLYEAR as i32 {
        let li = y + if 1 < yourtm.tm_mon { 1 } else { 0 };
        yourtm.tm_mday -= YEAR_LENGTHS[is_leap_idx(li)] as i32;
        if increment_overflow32(&mut y, 1) {
            return Err(1);
        }
    }
    loop {
        let i = MON_LENGTHS[is_leap_idx(y)][yourtm.tm_mon as usize];
        if yourtm.tm_mday <= i {
            break;
        }
        yourtm.tm_mday -= i;
        yourtm.tm_mon += 1;
        if yourtm.tm_mon >= MONSPERYEAR as i32 {
            yourtm.tm_mon = 0;
            if increment_overflow32(&mut y, 1) {
                return Err(1);
            }
        }
    }

    if increment_overflow32(&mut y, -(TM_YEAR_BASE as i32)) {
        return Err(1);
    }
    if y < i32::MIN as i64 || y > i32::MAX as i64 {
        return Err(1);
    }
    yourtm.tm_year = y as i32;

    let saved_seconds;
    if yourtm.tm_sec >= 0 && yourtm.tm_sec < SECSPERMIN as i32 {
        saved_seconds = 0;
    } else if yourtm.tm_year < (EPOCH_YEAR - TM_YEAR_BASE) as i32 {
        let delta = 1 - SECSPERMIN as i32;
        if (yourtm.tm_sec >= 0 && delta > i32::MAX - yourtm.tm_sec)
            || (yourtm.tm_sec < 0 && delta < i32::MIN - yourtm.tm_sec)
        {
            return Err(1);
        }
        yourtm.tm_sec += delta;
        saved_seconds = yourtm.tm_sec;
        yourtm.tm_sec = SECSPERMIN as i32 - 1;
    } else {
        saved_seconds = yourtm.tm_sec;
        yourtm.tm_sec = 0;
    }

    // Binary search for the matching time_t
    let mut lo: i64 = TIME_T_MIN;
    let mut hi: i64 = TIME_T_MAX;
    loop {
        let t = lo / 2 + hi / 2;
        let t = if t < lo {
            lo
        } else if t > hi {
            hi
        } else {
            t
        };

        let dir = if let Some(mytm) = localsub(sp, t) {
            tmcomp(&mytm, &yourtm)
        } else {
            if t > 0 {
                1
            } else {
                -1
            }
        };

        if dir != 0 {
            if t == lo {
                if t == TIME_T_MAX {
                    return Err(2);
                }
                lo = t + 1;
            } else if t == hi {
                if t == TIME_T_MIN {
                    return Err(2);
                }
                hi = t - 1;
            }
            if lo > hi {
                return Err(2);
            }
            if dir > 0 {
                hi = t;
            } else {
                lo = t;
            }
            continue;
        }

        // Match found. Check DST preference.
        if yourtm.tm_isdst < 0 {
            let final_t = t.wrapping_add(saved_seconds as i64);
            return localsub(sp, final_t)
                .map(|normalized| (final_t, normalized))
                .ok_or(2);
        }

        if let Some(mytm) = localsub(sp, t) {
            if mytm.tm_isdst == yourtm.tm_isdst {
                let final_t = t.wrapping_add(saved_seconds as i64);
                return localsub(sp, final_t)
                    .map(|normalized| (final_t, normalized))
                    .ok_or(2);
            }

            // Right time, wrong DST type. Hunt for right type.
            for i in (0..sp.typecnt.max(0) as usize).rev() {
                if (sp.ttis[i].tt_isdst != 0) != (yourtm.tm_isdst != 0) {
                    continue;
                }
                for j in (0..sp.typecnt.max(0) as usize).rev() {
                    if (sp.ttis[j].tt_isdst != 0) == (yourtm.tm_isdst != 0) {
                        continue;
                    }
                    if sp.ttunspecified(j) {
                        continue;
                    }
                    let newt = t + sp.ttis[j].tt_utoff as i64 - sp.ttis[i].tt_utoff as i64;
                    if let Some(mytm2) = localsub(sp, newt) {
                        if tmcomp(&mytm2, &yourtm) == 0 && mytm2.tm_isdst == yourtm.tm_isdst {
                            let final_t = newt.wrapping_add(saved_seconds as i64);
                            return localsub(sp, final_t)
                                .map(|normalized| (final_t, normalized))
                                .ok_or(2);
                        }
                    }
                }
            }
            return Err(2);
        }

        let final_t = t.wrapping_add(saved_seconds as i64);
        return localsub(sp, final_t)
            .map(|normalized| (final_t, normalized))
            .ok_or(2);
    }
}

/// Matching upstream `time2`: try without normalization first, then with.
fn time2(sp: &TzRule, tmp: &CalendarTimeInternal) -> Result<(i64, CalendarTimeInternal), u32> {
    time2sub(sp, tmp, false).or_else(|_| time2sub(sp, tmp, true))
}

/// Matching upstream `time1`: convert broken-down time to time_t.
fn time1(out_time: &mut i64, tmp: &mut CalendarTimeInternal, sp: &TzRule) -> u32 {
    if tmp.tm_isdst > 1 {
        tmp.tm_isdst = 1;
    }
    let first_result = match time2(sp, tmp) {
        Ok((time, normalized)) => {
            *out_time = time;
            *tmp = normalized;
            return 0;
        }
        Err(result) => result,
    };
    if tmp.tm_isdst < 0 {
        return first_result;
    }

    if sp.timecnt < 1 {
        return 2;
    }

    let mut seen = [false; TZ_MAX_TYPES];
    let mut nseen = 0usize;
    let mut type_list = [0u8; TZ_MAX_TYPES];

    for i in (0..sp.timecnt as usize).rev() {
        let ty = sp.types[i] as usize;
        if !seen[ty] && !sp.ttunspecified(ty) {
            seen[ty] = true;
            type_list[nseen] = sp.types[i];
            nseen += 1;
        }
    }

    if nseen < 1 {
        return 2;
    }

    for sameind in 0..nseen {
        let samei = type_list[sameind] as usize;
        if (sp.ttis[samei].tt_isdst != 0) != (tmp.tm_isdst != 0) {
            continue;
        }
        for otherind in 0..nseen {
            let otheri = type_list[otherind] as usize;
            if (sp.ttis[otheri].tt_isdst != 0) == (tmp.tm_isdst != 0) {
                continue;
            }
            let adjustment = sp.ttis[otheri].tt_utoff - sp.ttis[samei].tt_utoff;
            tmp.tm_sec = tmp.tm_sec.wrapping_add(adjustment);
            tmp.tm_isdst = i32::from(tmp.tm_isdst == 0);
            if let Ok((time, normalized)) = time2(sp, tmp) {
                *out_time = time;
                *tmp = normalized;
                return 0;
            }
            tmp.tm_sec = tmp.tm_sec.wrapping_sub(adjustment);
            tmp.tm_isdst = i32::from(tmp.tm_isdst == 0);
        }
    }

    2
}

/// Matching upstream `mktime_tzname`.
pub fn mktime_tzname(out_time: &mut i64, sp: &TzRule, tmp: &mut CalendarTimeInternal) -> u32 {
    time1(out_time, tmp, sp)
}

/// Minimal POSIX TZ string parser for extending transitions beyond the TZif data.
/// Matches the subset of upstream `tzparse` needed for the post-V2 footer string.
pub fn parse_posix_tz(tz_str: &[u8]) -> Option<TzRule> {
    let s = std::str::from_utf8(tz_str).ok()?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Parse standard zone name
    let (stdname, rest) = parse_tz_name(s)?;
    if stdname.is_empty() {
        return None;
    }

    // Parse standard offset
    let (stdoffset, rest) = parse_tz_offset(rest)?;

    if rest.is_empty() {
        // No DST: single type
        let mut rule = TzRule::zeroed();
        rule.typecnt = 1;
        rule.ttis[0] = TtInfo {
            tt_utoff: -stdoffset as i32,
            tt_isdst: 0,
            tt_desigidx: 0,
            ..TtInfo::default()
        };

        rule.chars[..stdname.len()].copy_from_slice(stdname.as_bytes());
        rule.charcnt = (stdname.len() + 1) as i32;
        return Some(rule);
    }

    // Parse DST zone name
    let (dstname, rest) = parse_tz_name(rest)?;
    if dstname.is_empty() {
        return None;
    }

    // Parse DST offset (optional; default = stdoffset - 1 hour)
    let (dstoffset, rest) = if !rest.is_empty() && !rest.starts_with(',') && !rest.starts_with(';')
    {
        parse_tz_offset(rest)?
    } else {
        (stdoffset - SECSPERHOUR, rest)
    };

    // Parse transition rules (,start,end)
    let rest = if rest.starts_with(',') || rest.starts_with(';') {
        &rest[1..]
    } else if rest.is_empty() {
        // Use default rule: M3.2.0,M11.1.0
        "M3.2.0,M11.1.0"
    } else {
        return None;
    };

    let (start_rule, rest) = parse_tz_rule(rest)?;
    if !rest.starts_with(',') {
        return None;
    }
    let (end_rule, _) = parse_tz_rule(&rest[1..])?;

    // Build transitions for a wide range of years (matching upstream behavior)
    let mut rule = TzRule::zeroed();
    rule.typecnt = 2;
    rule.ttis[0] = TtInfo {
        tt_utoff: -stdoffset as i32,
        tt_isdst: 0,
        tt_desigidx: 0,
        ..TtInfo::default()
    };
    rule.ttis[1] = TtInfo {
        tt_utoff: -dstoffset as i32,
        tt_isdst: 1,
        tt_desigidx: stdname.len() as i32 + 1,
        ..TtInfo::default()
    };

    // Build abbreviation chars
    rule.chars[..stdname.len()].copy_from_slice(stdname.as_bytes());
    let dst_start = stdname.len() + 1;
    rule.chars[dst_start..dst_start + dstname.len()].copy_from_slice(dstname.as_bytes());
    rule.charcnt = (stdname.len() + 1 + dstname.len() + 1) as i32;

    // Generate transitions for years around the current era
    let yearbeg = EPOCH_YEAR - YEARSPERREPEAT / 2;
    let yearlim = EPOCH_YEAR + YEARSPERREPEAT / 2;

    let mut janfirst: i64;
    // Compute janfirst for yearbeg
    {
        let mut jf: i64 = 0;
        for yr in EPOCH_YEAR..yearbeg {
            let yl = YEAR_LENGTHS[is_leap_idx(yr)];
            jf = jf.wrapping_add(yl * SECSPERDAY);
        }
        for yr in (yearbeg..EPOCH_YEAR).rev() {
            let yl = YEAR_LENGTHS[is_leap_idx(yr)];
            jf = jf.wrapping_sub(yl * SECSPERDAY);
        }
        janfirst = jf;
    }

    for year in yearbeg..yearlim {
        let starttime = transtime(year as i32, &start_rule, -stdoffset);
        let endtime = transtime(year as i32, &end_rule, -dstoffset);

        let yearsecs = YEAR_LENGTHS[is_leap_idx(year)] * SECSPERDAY;

        let reversed = endtime < starttime;
        let (t1, t1_type, t2, t2_type) = if reversed {
            (endtime, 0u8, starttime, 1u8)
        } else {
            (starttime, 1u8, endtime, 0u8)
        };

        if (rule.timecnt as usize) < TZ_MAX_TIMES {
            let at = janfirst.wrapping_add(t1);
            rule.ats[rule.timecnt as usize] = at;
            rule.types[rule.timecnt as usize] = t1_type;
            rule.timecnt += 1;
        }
        if (rule.timecnt as usize) < TZ_MAX_TIMES {
            let at = janfirst.wrapping_add(t2);
            rule.ats[rule.timecnt as usize] = at;
            rule.types[rule.timecnt as usize] = t2_type;
            rule.timecnt += 1;
        }

        janfirst = janfirst.wrapping_add(yearsecs);
    }

    Some(rule)
}

/// Compute the year-relative time (in seconds from Jan 1 00:00 UT) at which
/// a transition rule takes effect (matching upstream `transtime`).
fn transtime(year: i32, rule: &PosixTzRule, offset: i64) -> i64 {
    let leapyear = is_leap(year as i64);
    let value = match rule.r_type {
        RuleType::JulianDay => {
            let mut v = (rule.r_day as i64 - 1) * SECSPERDAY;
            if leapyear && rule.r_day >= 60 {
                v += SECSPERDAY;
            }
            v
        }
        RuleType::DayOfYear => rule.r_day as i64 * SECSPERDAY,
        RuleType::MonthNthDayOfWeek => {
            let leap_idx = if leapyear { 1 } else { 0 };
            let m1 = (rule.r_mon + 9) % 12 + 1;
            let yy0 = if rule.r_mon <= 2 { year - 1 } else { year };
            let yy1 = yy0 / 100;
            let yy2 = yy0 % 100;
            let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
            if dow < 0 {
                dow += DAYSPERWEEK as i32;
            }

            let mut d = rule.r_day - dow;
            if d < 0 {
                d += DAYSPERWEEK as i32;
            }
            for _ in 1..rule.r_week {
                if d + DAYSPERWEEK as i32 >= MON_LENGTHS[leap_idx][(rule.r_mon - 1) as usize] {
                    break;
                }
                d += DAYSPERWEEK as i32;
            }

            let mut v = d as i64 * SECSPERDAY;
            for i in 0..(rule.r_mon - 1) as usize {
                v += MON_LENGTHS[leap_idx][i] as i64 * SECSPERDAY;
            }
            v
        }
    };
    value + rule.r_time + offset
}

#[derive(Debug, Clone)]
enum RuleType {
    JulianDay,
    DayOfYear,
    MonthNthDayOfWeek,
}

#[derive(Debug, Clone)]
struct PosixTzRule {
    r_type: RuleType,
    r_day: i32,
    r_week: i32,
    r_mon: i32,
    r_time: i64,
}

/// Parse a timezone abbreviation name from a POSIX TZ string.
/// Returns (name, remaining_string).
fn parse_tz_name(s: &str) -> Option<(&str, &str)> {
    if s.starts_with('<') {
        // Quoted name
        let rest = &s[1..];
        let end = rest.find('>')?;
        Some((&rest[..end], &rest[end + 1..]))
    } else {
        let end = s
            .find(|c: char| c.is_ascii_digit() || c == ',' || c == '-' || c == '+')
            .unwrap_or(s.len());
        if end == 0 {
            return None;
        }
        Some((&s[..end], &s[end..]))
    }
}

/// Parse an offset in [+-]hh[:mm[:ss]] form. Returns (seconds, remaining_string).
fn parse_tz_offset(s: &str) -> Option<(i64, &str)> {
    let (neg, s) = if s.starts_with('-') {
        (true, &s[1..])
    } else if s.starts_with('+') {
        (false, &s[1..])
    } else {
        (false, s)
    };

    let (secs, rest) = parse_tz_secs(s)?;
    Some((if neg { -secs } else { secs }, rest))
}

/// Parse hh[:mm[:ss]] and return (total_seconds, remaining_string).
fn parse_tz_secs(s: &str) -> Option<(i64, &str)> {
    let (hours, rest) = parse_tz_num(s)?;
    let mut secs = hours as i64 * SECSPERHOUR;

    if rest.starts_with(':') {
        let (mins, rest2) = parse_tz_num(&rest[1..])?;
        secs += mins as i64 * SECSPERMIN;
        if rest2.starts_with(':') {
            let (sec, rest3) = parse_tz_num(&rest2[1..])?;
            secs += sec as i64;
            return Some((secs, rest3));
        }
        return Some((secs, rest2));
    }
    Some((secs, rest))
}

/// Parse a decimal number from the start of a string.
fn parse_tz_num(s: &str) -> Option<(i32, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let num: i32 = s[..end].parse().ok()?;
    Some((num, &s[end..]))
}

/// Parse a POSIX TZ transition rule (Jn, n, or Mm.n.d[/time]).
fn parse_tz_rule(s: &str) -> Option<(PosixTzRule, &str)> {
    let (rule, rest) = if s.starts_with('J') {
        let (day, rest) = parse_tz_num(&s[1..])?;
        (
            PosixTzRule {
                r_type: RuleType::JulianDay,
                r_day: day,
                r_week: 0,
                r_mon: 0,
                r_time: 2 * SECSPERHOUR,
            },
            rest,
        )
    } else if s.starts_with('M') {
        let (mon, rest) = parse_tz_num(&s[1..])?;
        if !rest.starts_with('.') {
            return None;
        }
        let (week, rest) = parse_tz_num(&rest[1..])?;
        if !rest.starts_with('.') {
            return None;
        }
        let (day, rest) = parse_tz_num(&rest[1..])?;
        (
            PosixTzRule {
                r_type: RuleType::MonthNthDayOfWeek,
                r_day: day,
                r_week: week,
                r_mon: mon,
                r_time: 2 * SECSPERHOUR,
            },
            rest,
        )
    } else if s.starts_with(|c: char| c.is_ascii_digit()) {
        let (day, rest) = parse_tz_num(s)?;
        (
            PosixTzRule {
                r_type: RuleType::DayOfYear,
                r_day: day,
                r_week: 0,
                r_mon: 0,
                r_time: 2 * SECSPERHOUR,
            },
            rest,
        )
    } else {
        return None;
    };

    // Check for /time suffix
    let (mut rule, rest) = (rule, rest);
    if rest.starts_with('/') {
        let (time, rest2) = parse_tz_offset(&rest[1..])?;
        rule.r_time = time;
        Some((rule, rest2))
    } else {
        Some((rule, rest))
    }
}

#[cfg(test)]
mod tests {
    use super::{TtInfo, TzRule};
    use crate::file_sys::system_archive::time_zone_binary::time_zone_binary;

    #[test]
    fn rule_binary_layout_matches_eden() {
        assert_eq!(core::mem::size_of::<TtInfo>(), 0x10);
        assert_eq!(core::mem::offset_of!(TtInfo, tt_utoff), 0x0);
        assert_eq!(core::mem::offset_of!(TtInfo, tt_isdst), 0x4);
        assert_eq!(core::mem::offset_of!(TtInfo, tt_desigidx), 0x8);
        assert_eq!(core::mem::offset_of!(TtInfo, tt_ttisstd), 0xC);
        assert_eq!(core::mem::offset_of!(TtInfo, tt_ttisut), 0xD);

        assert_eq!(core::mem::size_of::<TzRule>(), 0x4000);
        assert_eq!(core::mem::offset_of!(TzRule, timecnt), 0x0);
        assert_eq!(core::mem::offset_of!(TzRule, ats), 0x10);
        assert_eq!(core::mem::offset_of!(TzRule, types), 0x1F50);
        assert_eq!(core::mem::offset_of!(TzRule, ttis), 0x2338);
        assert_eq!(core::mem::offset_of!(TzRule, chars), 0x2B38);
        assert_eq!(core::mem::offset_of!(TzRule, default_type), 0x2D38);
        assert_eq!(core::mem::offset_of!(TzRule, _padding1), 0x2D3C);
    }

    #[test]
    fn rule_ipc_bytes_are_deterministic_and_support_unaligned_input() {
        let rule = TzRule::default();
        assert_eq!(rule.as_bytes().len(), 0x4000);
        assert!(rule._padding0.iter().all(|byte| *byte == 0));
        assert!(rule._padding1.iter().all(|byte| *byte == 0));
        assert!(rule.ttis[0]._padding0.iter().all(|byte| *byte == 0));
        assert!(rule.ttis[0]._padding1.iter().all(|byte| *byte == 0));

        let mut unaligned = vec![0xFF; 0x4001];
        unaligned[1..].copy_from_slice(rule.as_bytes());
        let decoded = TzRule::from_bytes(&unaligned[1..]).expect("fixed-size rule");
        assert_eq!(decoded.timecnt, rule.timecnt);
        assert_eq!(decoded.typecnt, rule.typecnt);
        assert_eq!(&decoded.chars[..4], &[0; 4]);
    }

    #[test]
    fn in_large_data_zero_fills_after_the_available_prefix() {
        let bytes = 2i32.to_ne_bytes();
        let rule = TzRule::from_prefix_bytes(&bytes);

        assert_eq!(rule.timecnt, 2);
        assert!(rule.as_bytes()[bytes.len()..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn parses_switch_header_counter_order() {
        let mut binary = vec![0u8; 0x2C + 6 + 4 + 1];
        binary[..5].copy_from_slice(b"TZif2");
        binary[20..24].copy_from_slice(&1i32.to_be_bytes()); // ttisutcnt
        binary[36..40].copy_from_slice(&1i32.to_be_bytes()); // typecnt
        binary[40..44].copy_from_slice(&4i32.to_be_bytes()); // charcnt
        binary[0x2C + 6..0x2C + 10].copy_from_slice(b"GMT\0");
        binary[0x2C + 10] = 1;

        let rule = TzRule::parse(&binary).expect("Switch rule with ttisut flag");
        assert_eq!(rule.ttis[0].tt_ttisstd, 0);
        assert_eq!(rule.ttis[0].tt_ttisut, 1);
    }

    #[test]
    fn parses_switch_single_block_tzif2_rule() {
        let archive = time_zone_binary().expect("embedded timezone archive");
        let binary = archive
            .get_file_relative("/zoneinfo/Etc/GMT")
            .expect("Etc/GMT rule")
            .read_all_bytes();

        let rule = TzRule::parse(&binary).expect("Switch TZif2 rule must parse");
        assert_eq!(rule.timecnt, 0);
        assert_eq!(rule.typecnt, 1);
        assert_eq!(rule.charcnt, 4);
        assert_eq!(rule.ttis[0].tt_utoff, 0);
        assert_eq!(&rule.chars[..4], b"GMT\0");
    }
}
