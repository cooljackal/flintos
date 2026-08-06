// SPDX-License-Identifier: Apache-2.0

//! Report where a Flint image's bytes went, per memory region.
//!
//! # Why this is a Rust program and not two lines of shell
//!
//! It used to be a shell script wrapping `xtensa-esp32-elf-size`. That meant
//! finding the Xtensa toolchain from whichever shell `make` happened to spawn,
//! and on Windows that can be Git Bash, MSYS make's own shell, or WSL bash --
//! which disagree about whether a path looks like `C:/Users/x`, `/c/Users/x`,
//! or `/mnt/c/Users/x`. The report silently vanished for anyone whose setup did
//! not match. Parsing the ELF here needs no external tool at all, and `cargo`
//! is by definition already present.
//!
//! # Why per region rather than per section
//!
//! A total says nothing useful. An ESP32 image is scattered across memories
//! with wildly different budgets -- 127 KiB of IRAM, 64 KiB of DRAM, megabytes
//! of flash -- and the one that runs out first is almost always IRAM or DRAM,
//! long before flash. What matters is how full each region is.
//!
//! Region bounds are parsed out of the linker script rather than duplicated
//! here, so this cannot drift from the map the image was linked against.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let elf_path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: flint-size <elf> [linker-script]");
            return ExitCode::FAILURE;
        }
    };
    let ld_path = args
        .next()
        .unwrap_or_else(|| "arch/xtensa/flint32.ld".to_string());

    let elf = match std::fs::read(&elf_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("flint-size: {elf_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ld = std::fs::read_to_string(&ld_path).unwrap_or_default();

    let sections = match parse_sections(&elf) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("flint-size: {elf_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let regions = parse_regions(&ld);
    if regions.is_empty() {
        eprintln!("flint-size: no MEMORY regions found in {ld_path}");
        return ExitCode::FAILURE;
    }

    print!("{}", render(&elf_path, &sections, &regions));
    ExitCode::SUCCESS
}

// ── ELF ─────────────────────────────────────────────────────────────────────

/// One allocated section: what it costs and where it lives.
struct Section {
    name: String,
    addr: u64,
    size: u64,
    /// `false` for `.bss` and friends: they occupy RAM but no bytes in the
    /// flashed image.
    in_file: bool,
}

const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 0x2;

fn parse_sections(elf: &[u8]) -> Result<Vec<Section>, String> {
    if elf.len() < 52 || &elf[0..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if elf[4] != 1 {
        return Err("only 32-bit ELF is supported (ESP32 is ELF32)".into());
    }
    if elf[5] != 1 {
        return Err("only little-endian ELF is supported".into());
    }

    let u16at = |o: usize| u16::from_le_bytes([elf[o], elf[o + 1]]) as usize;
    let u32at = |o: usize| u32::from_le_bytes([elf[o], elf[o + 1], elf[o + 2], elf[o + 3]]);

    let shoff = u32at(0x20) as usize;
    let shentsize = u16at(0x2E);
    let shnum = u16at(0x30);
    let shstrndx = u16at(0x32);

    if shoff == 0 || shnum == 0 {
        return Err("no section headers".into());
    }
    // Every field this function reads sits within the first 0x18 bytes of an
    // entry, so an entry smaller than that cannot be indexed safely.
    if shentsize < 0x18 {
        return Err("section header entries are too small to be ELF32".into());
    }
    if shstrndx >= shnum {
        return Err("section-name string table index is out of range".into());
    }
    let need = shoff
        .checked_add(shnum.checked_mul(shentsize).ok_or("section table overflows")?)
        .ok_or("section table overflows")?;
    if need > elf.len() {
        return Err("section header table runs past the end of the file".into());
    }

    // The section-header string table, for names.
    let strtab = {
        let sh = shoff + shstrndx * shentsize;
        let off = u32at(sh + 0x10) as usize;
        let size = u32at(sh + 0x14) as usize;
        elf.get(off..off + size).unwrap_or(&[])
    };
    let name_at = |idx: usize| -> String {
        let end = strtab[idx..]
            .iter()
            .position(|&b| b == 0)
            .map_or(strtab.len(), |n| idx + n);
        String::from_utf8_lossy(&strtab[idx..end]).into_owned()
    };

    let mut out = Vec::new();
    for i in 0..shnum {
        let sh = shoff + i * shentsize;
        let sh_name = u32at(sh) as usize;
        let sh_type = u32at(sh + 0x04);
        let sh_flags = u32at(sh + 0x08) as u64;
        let sh_addr = u32at(sh + 0x0C) as u64;
        let sh_size = u32at(sh + 0x14) as u64;

        // Only allocated sections occupy the target's memory. Debug info lives
        // in the ELF but never reaches the chip.
        if sh_flags & SHF_ALLOC == 0 || sh_size == 0 {
            continue;
        }
        out.push(Section {
            name: if sh_name < strtab.len() {
                name_at(sh_name)
            } else {
                format!("<{i}>")
            },
            addr: sh_addr,
            size: sh_size,
            in_file: sh_type != SHT_NOBITS,
        });
    }
    Ok(out)
}

// ── Linker script ───────────────────────────────────────────────────────────

struct Region {
    name: String,
    origin: u64,
    length: u64,
}

/// Pull `name (RWX) : ORIGIN = 0x..., LENGTH = 0x...` out of the MEMORY block.
///
/// Deliberately forgiving about whitespace and attribute order, and it ignores
/// everything outside a line that matches -- comments in that block are prose,
/// not something worth writing a parser for.
fn parse_regions(ld: &str) -> Vec<Region> {
    let mut out = Vec::new();
    for line in ld.lines() {
        let line = line.trim();
        let Some((head, rest)) = line.split_once(':') else {
            continue;
        };
        if !rest.contains("ORIGIN") || !rest.contains("LENGTH") {
            continue;
        }
        // `name (RWX)` -> `name`
        let name = head.split('(').next().unwrap_or("").trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let (Some(origin), Some(length)) = (field(rest, "ORIGIN"), field(rest, "LENGTH")) else {
            continue;
        };
        out.push(Region {
            name: name.to_string(),
            origin,
            length,
        });
    }
    out.sort_by_key(|r| r.origin);
    out
}

/// Value of `KEY = <number>` within `s`, hex or decimal.
fn field(s: &str, key: &str) -> Option<u64> {
    let after = s.split_once(key)?.1;
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let tok: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == 'x' || *c == 'X')
        .collect();
    if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        tok.parse().ok()
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn human(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

const BAR_WIDTH: usize = 20;

/// Bytes available to the app in espflash's default 4 MB layout: the factory
/// partition at 0x10000. Override with your own table and this number changes.
const APP_PARTITION_BYTES: u64 = 0x3F_0000;

fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * BAR_WIDTH as f64).round().clamp(0.0, BAR_WIDTH as f64) as usize;
    // A region at 1% should not render as empty, and one at 99% should not
    // render as full -- both are the questions someone reads this table to ask.
    let filled = if pct > 0.0 { filled.max(1) } else { 0 };
    let filled = if pct < 100.0 {
        filled.min(BAR_WIDTH - 1)
    } else {
        filled
    };
    format!("{}{}", "#".repeat(filled), ".".repeat(BAR_WIDTH - filled))
}

/// Column widths, chosen so the table is a fixed shape regardless of content.
const W_NAME: usize = 14;
const W_USED: usize = 10;
const W_CAP: usize = 10;
const W_PCT: usize = 6;

fn rule() -> String {
    format!(
        "+{}+{}+{}+{}+{}+",
        "-".repeat(W_NAME + 2),
        "-".repeat(W_USED + 2),
        "-".repeat(W_CAP + 2),
        "-".repeat(BAR_WIDTH + 2),
        "-".repeat(W_PCT + 2),
    )
}

fn row(name: &str, used: &str, cap: &str, bar: &str, pct: &str) -> String {
    format!(
        "| {name:<W_NAME$} | {used:>W_USED$} | {cap:>W_CAP$} | {bar:<BAR_WIDTH$} | {pct:>W_PCT$} |\n"
    )
}

fn render(elf_path: &str, sections: &[Section], regions: &[Region]) -> String {
    // Attribute each section to a region. A section in none of them is a real
    // finding, not a rounding error -- it means the linker placed bytes
    // somewhere the memory map does not describe.
    let mut used: BTreeMap<&str, u64> = BTreeMap::new();
    let mut unmapped: Vec<&Section> = Vec::new();
    let mut flash_bytes = 0u64;

    for s in sections {
        if s.in_file {
            flash_bytes += s.size;
        }
        match regions
            .iter()
            .find(|r| s.addr >= r.origin && s.addr < r.origin + r.length)
        {
            Some(r) => *used.entry(r.name.as_str()).or_default() += s.size,
            None => unmapped.push(s),
        }
    }

    let mut out = String::new();
    let name = std::path::Path::new(elf_path)
        .file_name()
        .map_or(elf_path, |n| n.to_str().unwrap_or(elf_path));

    let _ = writeln!(out);
    let _ = writeln!(out, "  Flint image: {name}");
    let _ = writeln!(out, "{}", rule());
    let _ = write!(out, "{}", row("REGION", "USED", "CAPACITY", "USAGE", "FULL"));
    let _ = writeln!(out, "{}", rule());

    // Regions that are a reservation rather than a measurement: nothing in them
    // is stored in the image, so "100% full" means "fully reserved", not "out
    // of room". Flagging them stops the table reading as an alarm.
    let mut reserved = Vec::new();

    for r in regions {
        let u = used.get(r.name.as_str()).copied().unwrap_or(0);
        if u == 0 {
            continue; // a region nothing landed in is noise
        }
        let is_reserved = sections
            .iter()
            .filter(|s| s.addr >= r.origin && s.addr < r.origin + r.length)
            .all(|s| !s.in_file);
        let pct = 100.0 * u as f64 / r.length as f64;
        let label = if is_reserved {
            reserved.push(r.name.clone());
            format!("{} *", r.name)
        } else {
            r.name.clone()
        };
        let _ = write!(
            out,
            "{}",
            row(
                &label,
                &human(u),
                &human(r.length),
                &bar(pct),
                &format!("{pct:.1}%"),
            )
        );
    }

    // Everything actually stored in the image, across every region.
    let _ = writeln!(out, "{}", rule());
    let pct = 100.0 * flash_bytes as f64 / APP_PARTITION_BYTES as f64;
    let _ = write!(
        out,
        "{}",
        row(
            "flash total",
            &human(flash_bytes),
            &human(APP_PARTITION_BYTES),
            &bar(pct),
            &format!("{pct:.1}%"),
        )
    );
    let _ = writeln!(out, "{}", rule());

    for name in &reserved {
        let _ = writeln!(
            out,
            "  * {name} is reserved space, not usage. Per-task high-water marks"
        );
        let _ = writeln!(out, "    come from the kernel's debug::stack at runtime.");
    }

    // espflash reports a noticeably larger number and the difference is not
    // rounding: the ESP32 MMU maps flash in 64 KiB pages, so the image builder
    // inserts a padding segment to land the mapped DROM and IROM segments on a
    // page boundary. That padding is real flash but is not anyone's code, and
    // reporting it here would make every content change look noisier than it is.
    let _ = writeln!(
        out,
        "  espflash reports more: it 64 KiB-aligns the mapped segments and the"
    );
    let _ = writeln!(out, "  padding counts toward the flashed file.");

    if !unmapped.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  WARNING: {} allocated section(s) fall outside every MEMORY region.",
            unmapped.len()
        );
        let _ = writeln!(
            out,
            "  These still cost flash. Give them a region in the linker script."
        );
        for s in &unmapped {
            let _ = writeln!(
                out,
                "    {:<20} {:>10} at {:#010x}",
                s.name,
                human(s.size),
                s.addr
            );
        }
    }

    let _ = writeln!(out);
    out
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const LD: &str = r#"
MEMORY
{
  /* A comment mentioning ORIGIN and LENGTH should not parse as a region. */
  vectors_seg (RX) : ORIGIN = 0x40080000, LENGTH = 0x400
  iram_seg    (RX) : ORIGIN = 0x40080400, LENGTH = 0x1FC00
  dram_seg    (RW) : ORIGIN = 0x3FFB0000, LENGTH = 0x10000
}
"#;

    #[test]
    fn regions_parse_out_of_the_memory_block() {
        let r = parse_regions(LD);
        assert_eq!(r.len(), 3);
        // Sorted by origin, so DRAM comes first.
        assert_eq!(r[0].name, "dram_seg");
        assert_eq!(r[0].origin, 0x3FFB_0000);
        assert_eq!(r[0].length, 0x1_0000);
        assert_eq!(r[2].name, "iram_seg");
    }

    #[test]
    fn prose_containing_the_keywords_is_not_a_region() {
        // The real linker script has a long comment block between entries.
        let r = parse_regions("  /* ORIGIN and LENGTH are explained above: see the note. */");
        assert!(r.is_empty());
    }

    #[test]
    fn field_reads_hex_and_decimal() {
        assert_eq!(field("ORIGIN = 0x40080000,", "ORIGIN"), Some(0x4008_0000));
        assert_eq!(field("LENGTH = 1024", "LENGTH"), Some(1024));
        assert_eq!(field("LENGTH = 0x400 /* 1K */", "LENGTH"), Some(0x400));
        assert_eq!(field("nothing here", "ORIGIN"), None);
    }

    #[test]
    fn human_switches_units_at_the_right_points() {
        assert_eq!(human(963), "963 B");
        assert_eq!(human(1024), "1.0 KiB");
        assert_eq!(human(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn a_nearly_empty_region_still_shows_one_mark() {
        // Rounding 0.4% to zero would read as "nothing here", which is a
        // different claim from "a little".
        assert!(bar(0.4).starts_with('#'));
        assert_eq!(bar(0.0), ".".repeat(BAR_WIDTH));
    }

    #[test]
    fn a_nearly_full_region_is_not_drawn_as_full() {
        // 99.6% and 100% mean different things to someone deciding whether the
        // next feature fits.
        assert!(bar(99.6).contains('.'));
        assert_eq!(bar(100.0), "#".repeat(BAR_WIDTH));
    }

    #[test]
    fn rejects_input_that_is_not_elf32() {
        assert!(parse_sections(b"not an elf at all, really").is_err());
        let mut fake = vec![0u8; 64];
        fake[0..4].copy_from_slice(b"\x7fELF");
        fake[4] = 2; // 64-bit
        assert!(parse_sections(&fake).is_err());
    }

    #[test]
    fn malformed_headers_return_an_error_rather_than_panicking() {
        // A truncated or hostile ELF must not take the build down with an
        // index-out-of-bounds. This tool runs on every `make build`.
        let mut fake = vec![0u8; 64];
        fake[0..4].copy_from_slice(b"\x7fELF");
        fake[4] = 1; // 32-bit
        fake[5] = 1; // little-endian
        fake[0x20..0x24].copy_from_slice(&8u32.to_le_bytes()); // e_shoff
        fake[0x2E..0x30].copy_from_slice(&0u16.to_le_bytes()); // e_shentsize = 0
        fake[0x30..0x32].copy_from_slice(&1u16.to_le_bytes()); // e_shnum
        fake[0x32..0x34].copy_from_slice(&99u16.to_le_bytes()); // e_shstrndx, out of range
        assert!(parse_sections(&fake).is_err());
    }

    #[test]
    fn sections_outside_every_region_are_reported() {
        let regions = parse_regions(LD);
        let sections = vec![
            Section { name: ".text".into(), addr: 0x4008_0400, size: 512, in_file: true },
            Section { name: ".stray".into(), addr: 0x0000_0000, size: 4096, in_file: true },
        ];
        let out = render("demo", &sections, &regions);
        assert!(out.contains("WARNING"));
        assert!(out.contains(".stray"));
        // The mapped one is not flagged.
        assert!(!out.contains(".text  "));
    }

    #[test]
    fn bss_costs_ram_but_not_flash() {
        let regions = parse_regions(LD);
        let sections = vec![
            Section { name: ".data".into(), addr: 0x3FFB_0000, size: 1024, in_file: true },
            Section { name: ".bss".into(), addr: 0x3FFB_0400, size: 8192, in_file: false },
        ];
        let out = render("demo", &sections, &regions);
        // dram_seg carries both: 9 KiB of 64 KiB.
        assert!(out.contains("9.0 KiB"), "{out}");
        // The flash total counts only the 1 KiB that is actually stored.
        assert!(out.contains("1.0 KiB"), "{out}");
    }
}
