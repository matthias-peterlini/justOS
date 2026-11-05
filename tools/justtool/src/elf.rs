//! Just enough ELF64 to build kernel images and check driver modules.

pub struct Segment {
    pub vaddr: u64,
    pub offset: u64,
    pub filesz: u64,
    pub memsz: u64,
}

pub struct Elf<'a> {
    pub data: &'a [u8],
    pub kind: u16,
    pub entry: u64,
    pub segments: Vec<Segment>,
    pub dynamic: Option<(u64, u64)>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Elf<'a>, String> {
        if data.len() < 64 || &data[..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 {
            return Err("not a 64-bit little-endian ELF file".into());
        }
        if u16_at(data, 18) != 0x3E {
            return Err("not an x86-64 ELF file".into());
        }
        let kind = u16_at(data, 16);
        let entry = u64_at(data, 24);
        let phoff = u64_at(data, 32) as usize;
        let phentsize = u16_at(data, 54) as usize;
        let phnum = u16_at(data, 56) as usize;
        let mut segments = Vec::new();
        let mut dynamic = None;
        for i in 0..phnum {
            let p = phoff + i * phentsize;
            if p + 56 > data.len() {
                return Err("truncated program headers".into());
            }
            let t = u32_at(data, p);
            let (offset, vaddr, filesz, memsz) = (u64_at(data, p + 8), u64_at(data, p + 16), u64_at(data, p + 32), u64_at(data, p + 40));
            if t == 1 && memsz > 0 {
                if offset + filesz > data.len() as u64 {
                    return Err("segment outside the file".into());
                }
                segments.push(Segment { vaddr, offset, filesz, memsz });
            } else if t == 2 {
                dynamic = Some((offset, filesz));
            }
        }
        segments.sort_by_key(|s| s.vaddr);
        Ok(Elf { data, kind, entry, segments, dynamic })
    }

    /// The loadable segments laid out flat from the lowest address, and the
    /// size in memory including .bss.
    pub fn flatten(&self) -> Result<(u64, Vec<u8>, u64), String> {
        let base = self.segments.first().ok_or("no loadable segments")?.vaddr;
        let mut out = Vec::new();
        let mut mem_end = base;
        for s in &self.segments {
            let at = (s.vaddr - base) as usize;
            if s.filesz > 0 {
                if out.len() < at + s.filesz as usize {
                    out.resize(at + s.filesz as usize, 0);
                }
                out[at..at + s.filesz as usize].copy_from_slice(&self.data[s.offset as usize..(s.offset + s.filesz) as usize]);
            }
            mem_end = mem_end.max(s.vaddr + s.memsz);
        }
        Ok((base, out, mem_end - base))
    }

    /// Checks that a driver module needs no symbol imports: every dynamic
    /// relocation must be R_X86_64_RELATIVE (or NONE).
    pub fn check_module(&self) -> Result<(), String> {
        if self.kind != 3 {
            return Err("a driver module must be position-independent (ET_DYN)".into());
        }
        let Some((off, size)) = self.dynamic else { return Ok(()) };
        let (mut rela_vaddr, mut relasz, mut relaent) = (0, 0, 24);
        let mut i = off as usize;
        while i + 16 <= (off + size) as usize {
            let tag = u64_at(self.data, i);
            let val = u64_at(self.data, i + 8);
            match tag {
                0 => break,
                7 => rela_vaddr = val,
                8 => relasz = val,
                9 => relaent = val,
                23 => return Err("the module has PLT relocations (it imports functions)".into()),
                _ => {}
            }
            i += 16;
        }
        if relasz == 0 {
            return Ok(());
        }
        // Map the RELA address back to a file offset.
        let seg = self
            .segments
            .iter()
            .find(|s| rela_vaddr >= s.vaddr && rela_vaddr < s.vaddr + s.filesz)
            .ok_or("relocations outside the loadable segments")?;
        let start = (seg.offset + rela_vaddr - seg.vaddr) as usize;
        let mut r = start;
        while r + relaent as usize <= start + relasz as usize {
            let t = u32_at(self.data, r + 8);
            if t != 0 && t != 8 {
                return Err(format!("relocation type {t} found: the module must not import symbols"));
            }
            r += relaent as usize;
        }
        Ok(())
    }
}
