//! Translates one PowerPC (Gekko) function into a Rust function operating on `runtime::Ctx`.
//!
//! Functions with internal control flow become a `loop { match pc { .. } }` state machine with
//! one arm per basic block; straight-line functions are emitted as plain statements.
//! Guest GPRs, FPRs, paired-single slots, CR fields, CTR, CA, and the `mem` base pointer are
//! cached in local SSA variables so LLVM pins them in physical ARM64 registers across guest
//! memory stores and loop iterations.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

pub struct Func {
    pub addr: u32,
    pub size: u32,
    pub name: String,
    pub hle: Option<usize>,
}

#[derive(Default)]
pub struct Stats {
    pub insns: usize,
    pub looped: usize,
    pub bad_branches: Vec<(u32, u32)>,
    pub invalid: Vec<(u32, u32)>,
    pub fallthrough: usize,
}

#[derive(Clone, Copy, Default)]
struct RegUsage {
    gpr_read: u32,
    gpr_write: u32,
    fpr_read: u32,
    fpr_write: u32,
    ps_read: u32,
    ps_write: u32,
    cr_read: u8,
    cr_write: u8,
    ctr_read: bool,
    ctr_write: bool,
    ca_read: bool,
    ca_write: bool,
    uses_mem: bool,
}

fn mask(mb: u32, me: u32) -> u32 {
    let x = 0xFFFF_FFFFu32 >> mb;
    let y = 0xFFFF_FFFFu32 << (31 - me);
    if mb <= me {
        x & y
    } else {
        x | y
    }
}

struct Emitter<'a> {
    f: &'a Func,
    funcs: &'a BTreeMap<u32, Func>,
    looped: bool,
    /// Treat every store as a possible write to the GX pipe (set for GX library functions).
    pipe_all: bool,
    dry_run: bool,
    known: RegUsage,
    u: RegUsage,
    stats: &'a mut Stats,
}

impl Emitter<'_> {
    #[inline(always)]
    fn rr(&mut self, k: u32) -> String {
        self.u.gpr_read |= 1 << k;
        format!("r{k}")
    }
    #[inline(always)]
    fn wr(&mut self, k: u32) -> String {
        self.u.gpr_write |= 1 << k;
        format!("r{k}")
    }
    #[inline(always)]
    fn rf(&mut self, k: u32) -> String {
        self.u.fpr_read |= 1 << k;
        format!("f{k}")
    }
    #[inline(always)]
    fn wf(&mut self, k: u32) -> String {
        self.u.fpr_write |= 1 << k;
        format!("f{k}")
    }
    #[inline(always)]
    fn rps(&mut self, k: u32) -> String {
        self.u.ps_read |= 1 << k;
        format!("ps{k}")
    }
    #[inline(always)]
    fn wps(&mut self, k: u32) -> String {
        self.u.ps_write |= 1 << k;
        format!("ps{k}")
    }
    #[inline(always)]
    fn rcr(&mut self, k: u32) -> String {
        self.u.cr_read |= 1 << k;
        format!("cr{k}")
    }
    #[inline(always)]
    fn wcr(&mut self, k: u32) -> String {
        self.u.cr_write |= 1 << k;
        format!("cr{k}")
    }
    #[inline(always)]
    fn rctr(&mut self) -> &'static str {
        self.u.ctr_read = true;
        "ctr"
    }
    #[inline(always)]
    fn wctr(&mut self) -> &'static str {
        self.u.ctr_write = true;
        "ctr"
    }
    #[inline(always)]
    fn rca(&mut self) -> &'static str {
        self.u.ca_read = true;
        "ca"
    }
    #[inline(always)]
    fn wca(&mut self) -> &'static str {
        self.u.ca_write = true;
        "ca"
    }
    #[inline(always)]
    fn umem(&mut self) -> &'static str {
        self.u.uses_mem = true;
        "mem"
    }

    fn flush_stmt(&self) -> String {
        if self.dry_run {
            return String::new();
        }
        let mut s = String::new();
        for k in 0..32 {
            if (self.known.gpr_write >> k) & 1 != 0 {
                write!(s, "c.r[{k}] = r{k}; ").unwrap();
            }
        }
        for k in 0..32 {
            if (self.known.fpr_write >> k) & 1 != 0 {
                write!(s, "c.f[{k}] = f{k}; ").unwrap();
            }
        }
        for k in 0..32 {
            if (self.known.ps_write >> k) & 1 != 0 {
                write!(s, "c.ps1[{k}] = ps{k}; ").unwrap();
            }
        }
        for k in 0..8 {
            if (self.known.cr_write >> k) & 1 != 0 {
                write!(s, "c.cr[{k}] = cr{k}; ").unwrap();
            }
        }
        if self.known.ctr_write {
            s.push_str("c.ctr = ctr; ");
        }
        if self.known.ca_write {
            s.push_str("c.ca = ca; ");
        }
        s
    }

    fn reload_stmt(&self) -> String {
        if self.dry_run {
            return String::new();
        }
        let mut s = String::new();
        let gpr_used = self.known.gpr_read | self.known.gpr_write;
        for k in 0..32 {
            if (gpr_used >> k) & 1 != 0 {
                write!(s, "r{k} = c.r[{k}]; ").unwrap();
            }
        }
        let fpr_used = self.known.fpr_read | self.known.fpr_write;
        for k in 0..32 {
            if (fpr_used >> k) & 1 != 0 {
                write!(s, "f{k} = c.f[{k}]; ").unwrap();
            }
        }
        let ps_used = self.known.ps_read | self.known.ps_write;
        for k in 0..32 {
            if (ps_used >> k) & 1 != 0 {
                write!(s, "ps{k} = c.ps1[{k}]; ").unwrap();
            }
        }
        let cr_used = self.known.cr_read | self.known.cr_write;
        for k in 0..8 {
            if (cr_used >> k) & 1 != 0 {
                write!(s, "cr{k} = c.cr[{k}]; ").unwrap();
            }
        }
        if self.known.ctr_read || self.known.ctr_write {
            s.push_str("ctr = c.ctr; ");
        }
        if self.known.ca_read || self.known.ca_write {
            s.push_str("ca = c.ca; ");
        }
        s
    }

    fn ea_d(&mut self, a: u32, d: i32) -> String {
        match (a, d) {
            (0, _) => format!("0x{:X}u32", d as u32),
            (_, 0) => self.rr(a),
            _ => format!("{}.wrapping_add(0x{:X})", self.rr(a), d as u32),
        }
    }

    fn ea_x(&mut self, a: u32, b: u32) -> String {
        if a == 0 {
            self.rr(b)
        } else {
            let ra = self.rr(a);
            let rb = self.rr(b);
            format!("{ra}.wrapping_add({rb})")
        }
    }

    /// A load or store at `ea`; `update` writes the effective address back to rA afterwards.
    fn mem_op(&mut self, kind: &str, reg: u32, ea: String, update: Option<u32>) -> String {
        let body = match kind {
            "lwz" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr32({m}, ea);")
            }
            "lbz" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr8({m}, ea) as u32;")
            }
            "lhz" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr16({m}, ea) as u32;")
            }
            "lha" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr16({m}, ea) as i16 as u32;")
            }
            "lwbr" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr32({m}, ea).swap_bytes();")
            }
            "lhbr" => {
                let m = self.umem();
                let rd = self.wr(reg);
                format!("{rd} = runtime::mr16({m}, ea).swap_bytes() as u32;")
            }
            "stw" => {
                let m = self.umem();
                let rs = self.rr(reg);
                format!("runtime::mw32({m}, ea, {rs});")
            }
            "stb" => {
                let m = self.umem();
                let rs = self.rr(reg);
                format!("runtime::mw8({m}, ea, {rs} as u8);")
            }
            "sth" => {
                let m = self.umem();
                let rs = self.rr(reg);
                format!("runtime::mw16({m}, ea, {rs} as u16);")
            }
            "stwbr" => {
                let m = self.umem();
                let rs = self.rr(reg);
                format!("runtime::mw32({m}, ea, {rs}.swap_bytes());")
            }
            "sthbr" => {
                let m = self.umem();
                let rs = self.rr(reg);
                format!("runtime::mw16({m}, ea, ({rs} as u16).swap_bytes());")
            }
            "gstw" => {
                let rs = self.rr(reg);
                format!("c.gw32(ea, {rs});")
            }
            "gstb" => {
                let rs = self.rr(reg);
                format!("c.gw8(ea, {rs} as u8);")
            }
            "gsth" => {
                let rs = self.rr(reg);
                format!("c.gw16(ea, {rs} as u16);")
            }
            "lfs" => {
                let m = self.umem();
                let fd = self.wf(reg);
                let psd = self.wps(reg);
                format!("let v = f32::from_bits(runtime::mr32({m}, ea)) as f64; {fd} = v; {psd} = v;")
            }
            "lfd" => {
                let m = self.umem();
                let fd = self.wf(reg);
                format!("{fd} = f64::from_bits(runtime::mr64({m}, ea));")
            }
            "stfs" => {
                let m = self.umem();
                let fs = self.rf(reg);
                format!("runtime::mw32({m}, ea, ({fs} as f32).to_bits());")
            }
            "stfd" => {
                let m = self.umem();
                let fs = self.rf(reg);
                format!("runtime::mw64({m}, ea, {fs}.to_bits());")
            }
            "gstfs" => {
                let fs = self.rf(reg);
                format!("c.gw32(ea, ({fs} as f32).to_bits());")
            }
            "gstfd" => {
                let fs = self.rf(reg);
                format!("c.gw64(ea, {fs}.to_bits());")
            }
            "stfiw" => {
                let m = self.umem();
                let fs = self.rf(reg);
                format!("runtime::mw32({m}, ea, {fs}.to_bits() as u32);")
            }
            "lmw" => {
                let m = self.umem();
                let mut s = String::new();
                for (i, r) in (reg..32).enumerate() {
                    let rd = self.wr(r);
                    write!(s, "{rd} = runtime::mr32({m}, ea.wrapping_add(0x{:X})); ", i * 4).unwrap();
                }
                s
            }
            "stmw" => {
                let m = self.umem();
                let mut s = String::new();
                for (i, r) in (reg..32).enumerate() {
                    let rs = self.rr(r);
                    write!(s, "runtime::mw32({m}, ea.wrapping_add(0x{:X}), {rs}); ", i * 4).unwrap();
                }
                s
            }
            "dcbz" => {
                let m = self.umem();
                format!("runtime::dcbz_mem({m}, ea);")
            }
            _ => unreachable!("{kind}"),
        };
        match update {
            Some(a) => {
                let ra = self.wr(a);
                format!("{{ let ea = {ea}; {body} {ra} = ea; }}")
            }
            None => format!("{{ let ea = {ea}; {body} }}"),
        }
    }

    /// Splits a conditional-branch BO/BI pair into a statement to run first and a condition
    /// (`None` = branch always).
    fn branch_cond(&mut self, bo: u32, bi: u32) -> (String, Option<String>) {
        let mut pre = String::new();
        let mut parts = Vec::new();
        if bo & 4 == 0 {
            let ctr_r = self.rctr();
            let ctr_w = self.wctr();
            pre = format!("{ctr_w} = {ctr_r}.wrapping_sub(1); ");
            parts.push(if bo & 2 != 0 { format!("{ctr_w} == 0") } else { format!("{ctr_w} != 0") });
        }
        if bo & 16 == 0 {
            let op = if bo & 8 != 0 { "!=" } else { "==" };
            let cr = self.rcr(bi / 4);
            parts.push(format!("{cr} & {} {op} 0", 8 >> (bi % 4)));
        }
        (pre, if parts.is_empty() { None } else { Some(parts.join(" && ")) })
    }

    fn crbit_expr(&mut self, b: u32) -> String {
        let cr = self.rcr(b / 4);
        format!("({cr} & {} != 0)", 8 >> (b % 4))
    }

    fn rc_stmt(&mut self, val_expr: &str) -> String {
        let cr0 = self.wcr(0);
        format!("{cr0} = runtime::cmps_val({val_expr} as i32, 0);")
    }

    fn in_func(&self, t: u32) -> bool {
        t >= self.f.addr && t < self.f.addr + self.f.size
    }

    fn call(&mut self, pc: u32, t: u32) -> String {
        let fl = self.flush_stmt();
        let rl = self.reload_stmt();
        if self.funcs.contains_key(&t) {
            format!("{{ c.lr = 0x{:08X}; {fl}fn_{t:08X}(c); {rl}}}", pc + 4)
        } else {
            if !self.dry_run {
                self.stats.bad_branches.push((pc, t));
            }
            format!("{{ c.lr = 0x{:08X}; {fl}c.bad_branch(0x{pc:08X}, 0x{t:08X}); {rl}}}", pc + 4)
        }
    }

    fn jump(&mut self, pc: u32, t: u32) -> String {
        if self.in_func(t) && t <= pc {
            let fl = self.flush_stmt();
            let rl = self.reload_stmt();
            let faddr = self.f.addr;
            format!(
                "{{ if runtime::PROFILE_DETAILED {{ c.sample_tick_pc(0x{faddr:08X}); }} c.budget -= 1; if c.budget == 0 {{ {fl}c.poll(); {rl}}} pc = 0x{t:08X}; continue; }}"
            )
        } else if self.in_func(t) {
            format!("{{ pc = 0x{t:08X}; continue; }}")
        } else if self.funcs.contains_key(&t) {
            let fl = self.flush_stmt();
            format!("{{ {fl}return fn_{t:08X}(c); }}")
        } else {
            if !self.dry_run {
                self.stats.bad_branches.push((pc, t));
            }
            let fl = self.flush_stmt();
            format!("{{ {fl}return c.bad_branch(0x{pc:08X}, 0x{t:08X}); }}")
        }
    }

    /// Emits one instruction. Returns the statement and whether control never continues past it.
    fn insn(&mut self, pc: u32, w: u32) -> Option<(String, bool)> {
        let op = w >> 26;
        let (d, a, b, cc) = ((w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31, (w >> 6) & 31);
        let s = d;
        let simm = (w & 0xFFFF) as i16 as i32;
        let uimm = w & 0xFFFF;
        let rc = w & 1 != 0;
        let crf = d >> 2;
        let ok = |s: String| Some((s, false));

        match op {
            7 => {
                let ra = self.rr(a);
                let rd = self.wr(d);
                ok(format!("{rd} = ({ra} as i32).wrapping_mul({simm}) as u32;"))
            }
            8 => {
                let ra = self.rr(a);
                let rd = self.wr(d);
                let ca = self.wca();
                ok(format!("({rd}, {ca}) = runtime::add3_val(!{ra}, 0x{:X}, 1);", simm as u32))
            }
            10 => {
                let ra = self.rr(a);
                let cr = self.wcr(crf);
                ok(format!("{cr} = runtime::cmpu_val({ra}, 0x{uimm:X});"))
            }
            11 => {
                let ra = self.rr(a);
                let cr = self.wcr(crf);
                ok(format!("{cr} = runtime::cmps_val({ra} as i32, {simm});"))
            }
            12 => {
                let ra = self.rr(a);
                let rd = self.wr(d);
                let ca = self.wca();
                ok(format!("({rd}, {ca}) = runtime::add3_val({ra}, 0x{:X}, 0);", simm as u32))
            }
            13 => {
                let ra = self.rr(a);
                let rd = self.wr(d);
                let ca = self.wca();
                let rc_s = self.rc_stmt(&rd);
                ok(format!("({rd}, {ca}) = runtime::add3_val({ra}, 0x{:X}, 0); {rc_s}", simm as u32))
            }
            14 => {
                let ea = self.ea_d(a, simm);
                let rd = self.wr(d);
                ok(format!("{rd} = {ea};"))
            }
            15 => {
                let ea = self.ea_d(a, simm << 16);
                let rd = self.wr(d);
                ok(format!("{rd} = {ea};"))
            }
            16 => {
                let t = if w & 2 != 0 { (w & 0xFFFC) as i16 as i32 as u32 } else { pc.wrapping_add((w & 0xFFFC) as i16 as i32 as u32) };
                let (pre, cond) = self.branch_cond(d, a);
                let act = if w & 1 != 0 { self.call(pc, t) } else { self.jump(pc, t) };
                match cond {
                    Some(cond) => ok(format!("{pre}if {cond} {{ {act} }}")),
                    None => Some((format!("{pre}{act}"), w & 1 == 0)),
                }
            }
            17 => {
                let fl = self.flush_stmt();
                ok(format!("{{ {fl}c.trap(\"sc\"); }}"))
            }
            18 => {
                let li = ((w & 0x03FF_FFFC) << 6) as i32 >> 6;
                let t = if w & 2 != 0 { li as u32 } else { pc.wrapping_add(li as u32) };
                if w & 1 != 0 {
                    ok(self.call(pc, t))
                } else {
                    Some((self.jump(pc, t), true))
                }
            }
            19 => self.op19(pc, w),
            20 => {
                let rs = self.rr(s);
                let ra_in = self.rr(a);
                let ra = self.wr(a);
                let m = mask(cc, (w >> 1) & 31);
                let n = !m;
                let stmt = format!("{ra} = ({rs}.rotate_left({b}) & 0x{m:X}) | ({ra_in} & 0x{n:X});");
                ok(if rc { format!("{stmt} {}", self.rc_stmt(&ra)) } else { stmt })
            }
            21 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                let m = mask(cc, (w >> 1) & 31);
                let stmt = format!("{ra} = {rs}.rotate_left({b}) & 0x{m:X};");
                ok(if rc { format!("{stmt} {}", self.rc_stmt(&ra)) } else { stmt })
            }
            23 => {
                let rs = self.rr(s);
                let rb = self.rr(b);
                let ra = self.wr(a);
                let m = mask(cc, (w >> 1) & 31);
                let stmt = format!("{ra} = {rs}.rotate_left({rb} & 31) & 0x{m:X};");
                ok(if rc { format!("{stmt} {}", self.rc_stmt(&ra)) } else { stmt })
            }
            24 if w == 0x6000_0000 => ok(String::new()),
            24 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                ok(format!("{ra} = {rs} | 0x{uimm:X};"))
            }
            25 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                ok(format!("{ra} = {rs} | 0x{:X};", uimm << 16))
            }
            26 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                ok(format!("{ra} = {rs} ^ 0x{uimm:X};"))
            }
            27 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                ok(format!("{ra} = {rs} ^ 0x{:X};", uimm << 16))
            }
            28 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                let rc_s = self.rc_stmt(&ra);
                ok(format!("{ra} = {rs} & 0x{uimm:X}; {rc_s}"))
            }
            29 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                let rc_s = self.rc_stmt(&ra);
                ok(format!("{ra} = {rs} & 0x{:X}; {rc_s}", uimm << 16))
            }
            31 => self.op31(w).map(|s| (s, false)),
            32..=55 => {
                const KINDS: [&str; 12] = ["lwz", "lbz", "stw", "stb", "lhz", "lha", "sth", "", "lfs", "lfd", "stfs", "stfd"];
                let kind = match op {
                    46 => "lmw",
                    47 => "stmw",
                    _ => KINDS[((op - 32) / 2) as usize],
                };
                let update = (op & 1 != 0 && op < 46 || op >= 48 && op & 1 != 0).then_some(a);
                let piped = format!("g{kind}");
                let may_pipe = simm == -0x8000 && a != 0 || self.pipe_all;
                let kind = if may_pipe && kind.starts_with("st") && kind != "stmw" { &piped } else { kind };
                let ea = self.ea_d(a, simm);
                ok(self.mem_op(kind, d, ea, update))
            }
            56 | 57 | 60 | 61 => {
                let d12 = ((w & 0xFFF) << 20) as i32 >> 20;
                let single = (w >> 15) & 1 != 0;
                let qi = (w >> 12) & 7;
                let ea = self.ea_d(a, d12);
                let m = self.umem();
                let call = if op < 60 {
                    let fd = self.wf(d);
                    let psd = self.wps(d);
                    format!("({fd}, {psd}) = c.psq_l_val({m}, ea, {single}, {qi});")
                } else {
                    let fs = self.rf(d);
                    let pss = if single { "0.0".to_string() } else { self.rps(d) };
                    format!("c.psq_st_val({m}, ea, {fs}, {pss}, {single}, {qi});")
                };
                let upd = if op & 1 != 0 {
                    let ra = self.wr(a);
                    format!(" {ra} = ea;")
                } else {
                    String::new()
                };
                ok(format!("{{ let ea = {ea}; {call}{upd} }}"))
            }
            59 => {
                let e = match (w >> 1) & 31 {
                    18 => format!("{} / {}", self.rf(a), self.rf(b)),
                    20 => format!("{} - {}", self.rf(a), self.rf(b)),
                    21 => format!("{} + {}", self.rf(a), self.rf(b)),
                    22 => format!("{}.sqrt()", self.rf(b)),
                    24 => format!("1.0 / {}", self.rf(b)),
                    25 => format!("{} * {}", self.rf(a), self.rf(cc)),
                    28 => format!("{} * {} - {}", self.rf(a), self.rf(cc), self.rf(b)),
                    29 => format!("{} * {} + {}", self.rf(a), self.rf(cc), self.rf(b)),
                    30 => format!("-({} * {} - {})", self.rf(a), self.rf(cc), self.rf(b)),
                    31 => format!("-({} * {} + {})", self.rf(a), self.rf(cc), self.rf(b)),
                    _ => return None,
                };
                let fd = self.wf(d);
                let psd = self.wps(d);
                ok(format!("{{ let v = ({e}) as f32 as f64; {fd} = v; {psd} = v; }}"))
            }
            63 => self.op63(w).map(|s| (s, false)),
            4 => self.op4(w).map(|s| (s, false)),
            _ => None,
        }
    }

    fn op19(&mut self, pc: u32, w: u32) -> Option<(String, bool)> {
        let (d, a, b) = ((w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31);
        let lk = w & 1 != 0;
        let ok = |s: String| Some((s, false));
        match (w >> 1) & 0x3FF {
            0 => {
                let cra = self.rcr(a >> 2);
                let crd = self.wcr(d >> 2);
                ok(format!("{crd} = {cra};"))
            }
            16 => {
                let (pre, cond) = self.branch_cond(d, a);
                let fl = self.flush_stmt();
                let act = if lk {
                    let rl = self.reload_stmt();
                    format!("{{ let t = c.lr; c.lr = 0x{:08X}; {fl}c.call(t); {rl}}}", pc + 4)
                } else {
                    format!("{{ {fl}return; }}")
                };
                match cond {
                    Some(cond) => ok(format!("{pre}if {cond} {{ {act} }}")),
                    None => Some((format!("{pre}{act}"), !lk)),
                }
            }
            528 => {
                let (_, cond) = self.branch_cond(d | 4, a);
                let ctr = self.rctr();
                let fl = self.flush_stmt();
                let act = if lk {
                    let rl = self.reload_stmt();
                    format!("{{ let t = {ctr}; c.lr = 0x{:08X}; {fl}c.call(t); {rl}}}", pc + 4)
                } else if self.looped {
                    format!(
                        "{{ let t = {ctr}; if t.wrapping_sub(0x{:08X}) < 0x{:X} {{ pc = t; continue; }} {fl}return c.call(t); }}",
                        self.f.addr, self.f.size
                    )
                } else {
                    format!("{{ let t = {ctr}; {fl}return c.call(t); }}")
                };
                match cond {
                    Some(cond) => ok(format!("if {cond} {{ {act} }}")),
                    None => Some((act, !lk)),
                }
            }
            xo @ (33 | 129 | 193 | 225 | 257 | 289 | 417 | 449) => {
                let ca = self.crbit_expr(a);
                let cb = self.crbit_expr(b);
                let e = match xo {
                    33 => format!("!({ca} | {cb})"),
                    129 => format!("{ca} & !{cb}"),
                    193 => format!("{ca} ^ {cb}"),
                    225 => format!("!({ca} & {cb})"),
                    257 => format!("{ca} & {cb}"),
                    289 => format!("!({ca} ^ {cb})"),
                    417 => format!("{ca} | !{cb}"),
                    449 => format!("{ca} | {cb}"),
                    _ => unreachable!(),
                };
                let crd_r = self.rcr(d / 4);
                let crd_w = self.wcr(d / 4);
                let m = 8u8 >> (d % 4);
                let nm = !m;
                ok(format!("{{ let v = {e}; {crd_w} = if v {{ {crd_r} | {m} }} else {{ {crd_r} & 0x{nm:X} }}; }}"))
            }
            50 => {
                let fl = self.flush_stmt();
                ok(format!("{{ {fl}c.trap(\"rfi\"); }}"))
            }
            150 => ok(String::new()),
            _ => None,
        }
    }

    fn op31(&mut self, w: u32) -> Option<String> {
        let (d, a, b) = ((w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31);
        let s = d;
        let rc = w & 1 != 0;
        let crf = d >> 2;
        let mut xo = (w >> 1) & 0x3FF;
        if [8, 10, 40, 104, 136, 138, 200, 202, 232, 234, 235, 266, 459, 491].contains(&(xo & 0x1FF)) {
            xo &= 0x1FF;
        }
        let pipe_all = self.pipe_all;
        let spr = ((w >> 16) & 0x1F) | ((w >> 6) & 0x3E0);

        let finish_d = |this: &mut Self, expr: String| -> Option<String> {
            let rd = this.wr(d);
            Some(if rc {
                let rc_s = this.rc_stmt(&rd);
                format!("{rd} = {expr}; {rc_s}")
            } else {
                format!("{rd} = {expr};")
            })
        };
        let finish_d_ca = |this: &mut Self, expr: String| -> Option<String> {
            let rd = this.wr(d);
            let ca = this.wca();
            Some(if rc {
                let rc_s = this.rc_stmt(&rd);
                format!("({rd}, {ca}) = {expr}; {rc_s}")
            } else {
                format!("({rd}, {ca}) = {expr};")
            })
        };
        let finish_a = |this: &mut Self, expr: String| -> Option<String> {
            let ra = this.wr(a);
            Some(if rc {
                let rc_s = this.rc_stmt(&ra);
                format!("{ra} = {expr}; {rc_s}")
            } else {
                format!("{ra} = {expr};")
            })
        };
        let ld = |this: &mut Self, kind: &str, upd: bool| -> Option<String> {
            let piped = format!("g{kind}");
            let kind = if pipe_all && matches!(kind, "stw" | "stb" | "sth" | "stfs" | "stfd") { &piped } else { kind };
            let ea = this.ea_x(a, b);
            Some(this.mem_op(kind, d, ea, upd.then_some(a)))
        };

        match xo {
            0 => {
                let ra = self.rr(a);
                let rb = self.rr(b);
                let cr = self.wcr(crf);
                Some(format!("{cr} = runtime::cmps_val({ra} as i32, {rb} as i32);"))
            }
            32 => {
                let ra = self.rr(a);
                let rb = self.rr(b);
                let cr = self.wcr(crf);
                Some(format!("{cr} = runtime::cmpu_val({ra}, {rb});"))
            }
            8 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d_ca(self, format!("runtime::add3_val(!{ra}, {rb}, 1)"))
            }
            10 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d_ca(self, format!("runtime::add3_val({ra}, {rb}, 0)"))
            }
            11 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("(({ra} as u64 * {rb} as u64) >> 32) as u32"))
            }
            75 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("(({ra} as i32 as i64 * {rb} as i32 as i64) >> 32) as u32"))
            }
            40 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("{rb}.wrapping_sub({ra})"))
            }
            104 => {
                let ra = self.rr(a);
                finish_d(self, format!("{ra}.wrapping_neg()"))
            }
            136 => {
                let (ra, rb, ca_in) = (self.rr(a), self.rr(b), self.rca());
                finish_d_ca(self, format!("runtime::add3_val(!{ra}, {rb}, {ca_in})"))
            }
            138 => {
                let (ra, rb, ca_in) = (self.rr(a), self.rr(b), self.rca());
                finish_d_ca(self, format!("runtime::add3_val({ra}, {rb}, {ca_in})"))
            }
            200 => {
                let (ra, ca_in) = (self.rr(a), self.rca());
                finish_d_ca(self, format!("runtime::add3_val(!{ra}, 0, {ca_in})"))
            }
            202 => {
                let (ra, ca_in) = (self.rr(a), self.rca());
                finish_d_ca(self, format!("runtime::add3_val({ra}, 0, {ca_in})"))
            }
            232 => {
                let (ra, ca_in) = (self.rr(a), self.rca());
                finish_d_ca(self, format!("runtime::add3_val(!{ra}, 0xFFFFFFFF, {ca_in})"))
            }
            234 => {
                let (ra, ca_in) = (self.rr(a), self.rca());
                finish_d_ca(self, format!("runtime::add3_val({ra}, 0xFFFFFFFF, {ca_in})"))
            }
            235 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("{ra}.wrapping_mul({rb})"))
            }
            266 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("{ra}.wrapping_add({rb})"))
            }
            459 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("{ra}.checked_div({rb}).unwrap_or(0)"))
            }
            491 => {
                let (ra, rb) = (self.rr(a), self.rr(b));
                finish_d(self, format!("({ra} as i32).checked_div({rb} as i32).unwrap_or(0) as u32"))
            }
            19 => {
                let mut parts = Vec::with_capacity(8);
                for i in 0..8 {
                    let cr = self.rcr(i);
                    let sh = 28 - 4 * i;
                    if sh > 0 {
                        parts.push(format!("(({cr} as u32) << {sh})"));
                    } else {
                        parts.push(format!("({cr} as u32)"));
                    }
                }
                let rd = self.wr(d);
                Some(format!("{rd} = {};", parts.join(" | ")))
            }
            144 => {
                let m = (w >> 12) & 0xFF;
                let rs = self.rr(s);
                let mut stmt = format!("{{ let v = {rs}; ");
                for i in 0..8 {
                    if m & (0x80 >> i) != 0 {
                        let cr = self.wcr(i);
                        write!(stmt, "{cr} = ((v >> {}) & 0xF) as u8; ", 28 - 4 * i).unwrap();
                    }
                }
                stmt.push('}');
                Some(stmt)
            }
            83 => {
                let rd = self.wr(d);
                Some(format!("{rd} = c.msr;"))
            }
            146 => {
                let rs = self.rr(s);
                Some(format!("c.msr = {rs};"))
            }
            595 => {
                let rd = self.wr(d);
                Some(format!("{rd} = 0;"))
            }
            339 => {
                let val = match spr {
                    1 => format!("c.xer_rest | ({} << 29)", self.rca()),
                    8 => "c.lr".to_string(),
                    9 => self.rctr().to_string(),
                    _ => format!("c.spr[{spr}]"),
                };
                let rd = self.wr(d);
                Some(format!("{rd} = {val};"))
            }
            467 => match spr {
                1 => {
                    let rs = self.rr(s);
                    let ca = self.wca();
                    Some(format!("{{ let v = {rs}; {ca} = (v >> 29) & 1; c.xer_rest = v & !(1 << 29); }}"))
                }
                8 => {
                    let rs = self.rr(s);
                    Some(format!("c.lr = {rs};"))
                }
                9 => {
                    let rs = self.rr(s);
                    let ctr = self.wctr();
                    Some(format!("{ctr} = {rs};"))
                }
                _ => {
                    let rs = self.rr(s);
                    Some(format!("c.spr[{spr}] = {rs};"))
                }
            },
            371 => {
                let rd = self.wr(d);
                Some(format!("{rd} = (c.timebase() >> {}) as u32;", if spr == 269 { 32 } else { 0 }))
            }
            24 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("runtime::slw_val({rs}, {rb})"))
            }
            536 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("runtime::srw_val({rs}, {rb})"))
            }
            792 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                let ra = self.wr(a);
                let ca = self.wca();
                Some(if rc {
                    let rc_s = self.rc_stmt(&ra);
                    format!("({ra}, {ca}) = runtime::sraw_val({rs}, {rb}); {rc_s}")
                } else {
                    format!("({ra}, {ca}) = runtime::sraw_val({rs}, {rb});")
                })
            }
            824 => {
                let rs = self.rr(s);
                let ra = self.wr(a);
                let ca = self.wca();
                Some(if rc {
                    let rc_s = self.rc_stmt(&ra);
                    format!("({ra}, {ca}) = runtime::sraw_val({rs}, {b}); {rc_s}")
                } else {
                    format!("({ra}, {ca}) = runtime::sraw_val({rs}, {b});")
                })
            }
            26 => {
                let rs = self.rr(s);
                finish_a(self, format!("{rs}.leading_zeros()"))
            }
            28 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("{rs} & {rb}"))
            }
            60 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("{rs} & !{rb}"))
            }
            124 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("!({rs} | {rb})"))
            }
            284 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("!({rs} ^ {rb})"))
            }
            316 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("{rs} ^ {rb}"))
            }
            412 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("{rs} | !{rb}"))
            }
            444 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("{rs} | {rb}"))
            }
            476 => {
                let (rs, rb) = (self.rr(s), self.rr(b));
                finish_a(self, format!("!({rs} & {rb})"))
            }
            922 => {
                let rs = self.rr(s);
                finish_a(self, format!("{rs} as i16 as u32"))
            }
            954 => {
                let rs = self.rr(s);
                finish_a(self, format!("{rs} as i8 as u32"))
            }
            20 | 23 => ld(self, "lwz", false),
            55 => ld(self, "lwz", true),
            87 => ld(self, "lbz", false),
            119 => ld(self, "lbz", true),
            279 => ld(self, "lhz", false),
            311 => ld(self, "lhz", true),
            343 => ld(self, "lha", false),
            375 => ld(self, "lha", true),
            534 => ld(self, "lwbr", false),
            790 => ld(self, "lhbr", false),
            535 => ld(self, "lfs", false),
            567 => ld(self, "lfs", true),
            599 => ld(self, "lfd", false),
            631 => ld(self, "lfd", true),
            151 => ld(self, "stw", false),
            183 => ld(self, "stw", true),
            215 => ld(self, "stb", false),
            247 => ld(self, "stb", true),
            407 => ld(self, "sth", false),
            439 => ld(self, "sth", true),
            662 => ld(self, "stwbr", false),
            918 => ld(self, "sthbr", false),
            663 => ld(self, "stfs", false),
            695 => ld(self, "stfs", true),
            727 => ld(self, "stfd", false),
            759 => ld(self, "stfd", true),
            983 => ld(self, "stfiw", false),
            150 => {
                let ea = self.ea_x(a, b);
                let st = self.mem_op("stw", s, ea, None);
                let cr0 = self.wcr(0);
                Some(format!("{st} {cr0} = 2;"))
            }
            1014 => {
                let ea = self.ea_x(a, b);
                Some(self.mem_op("dcbz", 0, ea, None))
            }
            54 | 86 | 246 | 278 | 470 | 598 | 854 | 982 | 210 | 306 => Some(String::new()),
            _ => None,
        }
    }

    fn op63(&mut self, w: u32) -> Option<String> {
        let (d, a, b, cc) = ((w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31, (w >> 6) & 31);
        match (w >> 1) & 0x3FF {
            0 | 32 => {
                let (fa, fb) = (self.rf(a), self.rf(b));
                let cr = self.wcr(d >> 2);
                return Some(format!("{cr} = runtime::fcmp_val({fa}, {fb});"));
            }
            12 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                let psd = self.wps(d);
                return Some(format!("{{ let v = {fb} as f32 as f64; {fd} = v; {psd} = v; }}"));
            }
            14 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = runtime::fctiw_val({fb});"));
            }
            15 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = runtime::fctiwz_val({fb});"));
            }
            40 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = -{fb};"));
            }
            72 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = {fb};"));
            }
            136 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = -{fb}.abs();"));
            }
            264 => {
                let fb = self.rf(b);
                let fd = self.wf(d);
                return Some(format!("{fd} = {fb}.abs();"));
            }
            583 => {
                let fd = self.wf(d);
                return Some(format!("{fd} = f64::from_bits(c.fpscr as u64);"));
            }
            711 => {
                let fb = self.rf(b);
                return Some(format!("c.fpscr = {fb}.to_bits() as u32;"));
            }
            38 | 64 | 70 | 134 => return Some(String::new()),
            _ => {}
        }
        let e = match (w >> 1) & 31 {
            18 => format!("{} / {}", self.rf(a), self.rf(b)),
            20 => format!("{} - {}", self.rf(a), self.rf(b)),
            21 => format!("{} + {}", self.rf(a), self.rf(b)),
            22 => format!("{}.sqrt()", self.rf(b)),
            23 => format!("if {} >= 0.0 {{ {} }} else {{ {} }}", self.rf(a), self.rf(cc), self.rf(b)),
            25 => format!("{} * {}", self.rf(a), self.rf(cc)),
            26 => format!("1.0 / {}.sqrt()", self.rf(b)),
            28 => format!("{} * {} - {}", self.rf(a), self.rf(cc), self.rf(b)),
            29 => format!("{} * {} + {}", self.rf(a), self.rf(cc), self.rf(b)),
            30 => format!("-({} * {} - {})", self.rf(a), self.rf(cc), self.rf(b)),
            31 => format!("-({} * {} + {})", self.rf(a), self.rf(cc), self.rf(b)),
            _ => return None,
        };
        let fd = self.wf(d);
        Some(format!("{fd} = {e};"))
    }

    /// Gekko paired-single instructions.
    fn op4(&mut self, w: u32) -> Option<String> {
        let (d, a, b, cc) = ((w >> 21) & 31, (w >> 16) & 31, (w >> 11) & 31, (w >> 6) & 31);
        let finish_ps = |this: &mut Self, binds: String, e0: &str, e1: &str| -> Option<String> {
            let fd = this.wf(d);
            let psd = this.wps(d);
            Some(format!("{{ {binds} {fd} = ({e0}) as f32 as f64; {psd} = ({e1}) as f32 as f64; }}"))
        };
        match (w >> 1) & 31 {
            10 => {
                let binds = format!("let (a0, b1, c1) = ({}, {}, {});", self.rf(a), self.rps(b), self.rps(cc));
                return finish_ps(self, binds, "a0 + b1", "c1");
            }
            11 => {
                let binds = format!("let (a0, b1, c0) = ({}, {}, {});", self.rf(a), self.rps(b), self.rf(cc));
                return finish_ps(self, binds, "c0", "a0 + b1");
            }
            12 => {
                let binds = format!("let (a0, a1, c0) = ({}, {}, {});", self.rf(a), self.rps(a), self.rf(cc));
                return finish_ps(self, binds, "a0 * c0", "a1 * c0");
            }
            13 => {
                let binds = format!("let (a0, a1, c1) = ({}, {}, {});", self.rf(a), self.rps(a), self.rps(cc));
                return finish_ps(self, binds, "a0 * c1", "a1 * c1");
            }
            14 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0) = ({}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc)
                );
                return finish_ps(self, binds, "a0 * c0 + b0", "a1 * c0 + b1");
            }
            15 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c1) = ({}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rps(cc)
                );
                return finish_ps(self, binds, "a0 * c1 + b0", "a1 * c1 + b1");
            }
            18 => {
                let binds = format!("let (a0, a1, b0, b1) = ({}, {}, {}, {});", self.rf(a), self.rps(a), self.rf(b), self.rps(b));
                return finish_ps(self, binds, "a0 / b0", "a1 / b1");
            }
            20 => {
                let binds = format!("let (a0, a1, b0, b1) = ({}, {}, {}, {});", self.rf(a), self.rps(a), self.rf(b), self.rps(b));
                return finish_ps(self, binds, "a0 - b0", "a1 - b1");
            }
            21 => {
                let binds = format!("let (a0, a1, b0, b1) = ({}, {}, {}, {});", self.rf(a), self.rps(a), self.rf(b), self.rps(b));
                return finish_ps(self, binds, "a0 + b0", "a1 + b1");
            }
            23 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0, c1) = ({}, {}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc), self.rps(cc)
                );
                return finish_ps(self, binds, "if a0 >= 0.0 { c0 } else { b0 }", "if a1 >= 0.0 { c1 } else { b1 }");
            }
            24 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                return finish_ps(self, binds, "1.0 / b0", "1.0 / b1");
            }
            25 => {
                let binds = format!("let (a0, a1, c0, c1) = ({}, {}, {}, {});", self.rf(a), self.rps(a), self.rf(cc), self.rps(cc));
                return finish_ps(self, binds, "a0 * c0", "a1 * c1");
            }
            26 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                return finish_ps(self, binds, "1.0 / b0.sqrt()", "1.0 / b1.sqrt()");
            }
            28 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0, c1) = ({}, {}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc), self.rps(cc)
                );
                return finish_ps(self, binds, "a0 * c0 - b0", "a1 * c1 - b1");
            }
            29 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0, c1) = ({}, {}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc), self.rps(cc)
                );
                return finish_ps(self, binds, "a0 * c0 + b0", "a1 * c1 + b1");
            }
            30 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0, c1) = ({}, {}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc), self.rps(cc)
                );
                return finish_ps(self, binds, "-(a0 * c0 - b0)", "-(a1 * c1 - b1)");
            }
            31 => {
                let binds = format!(
                    "let (a0, a1, b0, b1, c0, c1) = ({}, {}, {}, {}, {}, {});",
                    self.rf(a), self.rps(a), self.rf(b), self.rps(b), self.rf(cc), self.rps(cc)
                );
                return finish_ps(self, binds, "-(a0 * c0 + b0)", "-(a1 * c1 + b1)");
            }
            _ => {}
        }
        let xo6 = (w >> 1) & 0x3F;
        if matches!(xo6, 6 | 7 | 38 | 39) {
            let single = (w >> 10) & 1 != 0;
            let qi = (w >> 7) & 7;
            let ea = self.ea_x(a, b);
            let m = self.umem();
            let call = if xo6 & 1 == 0 {
                let fd = self.wf(d);
                let psd = self.wps(d);
                format!("({fd}, {psd}) = c.psq_l_val({m}, ea, {single}, {qi});")
            } else {
                let fs = self.rf(d);
                let pss = if single { "0.0".to_string() } else { self.rps(d) };
                format!("c.psq_st_val({m}, ea, {fs}, {pss}, {single}, {qi});")
            };
            let upd = if xo6 >= 32 {
                let ra = self.wr(a);
                format!(" {ra} = ea;")
            } else {
                String::new()
            };
            return Some(format!("{{ let ea = {ea}; {call}{upd} }}"));
        }
        match (w >> 1) & 0x3FF {
            0 | 32 => {
                let (fa, fb) = (self.rf(a), self.rf(b));
                let cr = self.wcr(d >> 2);
                Some(format!("{cr} = runtime::fcmp_val({fa}, {fb});"))
            }
            64 | 96 => {
                let (psa, psb) = (self.rps(a), self.rps(b));
                let cr = self.wcr(d >> 2);
                Some(format!("{cr} = runtime::fcmp_val({psa}, {psb});"))
            }
            40 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                finish_ps(self, binds, "-b0", "-b1")
            }
            72 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                finish_ps(self, binds, "b0", "b1")
            }
            136 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                finish_ps(self, binds, "-b0.abs()", "-b1.abs()")
            }
            264 => {
                let binds = format!("let (b0, b1) = ({}, {});", self.rf(b), self.rps(b));
                finish_ps(self, binds, "b0.abs()", "b1.abs()")
            }
            528 => {
                let binds = format!("let (a0, b0) = ({}, {});", self.rf(a), self.rf(b));
                finish_ps(self, binds, "a0", "b0")
            }
            560 => {
                let binds = format!("let (a0, b1) = ({}, {});", self.rf(a), self.rps(b));
                finish_ps(self, binds, "a0", "b1")
            }
            592 => {
                let binds = format!("let (a1, b0) = ({}, {});", self.rps(a), self.rf(b));
                finish_ps(self, binds, "a1", "b0")
            }
            624 => {
                let binds = format!("let (a1, b1) = ({}, {});", self.rps(a), self.rps(b));
                finish_ps(self, binds, "a1", "b1")
            }
            1014 => {
                let ea = self.ea_x(a, b);
                Some(self.mem_op("dcbz", 0, ea, None))
            }
            _ => None,
        }
    }

    fn emit_body(&mut self, words: &[u32], leaders: &BTreeSet<u32>, out: &mut String) {
        let end = self.f.addr + self.f.size;
        let mut terminated = false;
        let indent = if self.looped { "            " } else { "    " };
        for (i, &w) in words.iter().enumerate() {
            let pc = self.f.addr + i as u32 * 4;
            if self.looped && (leaders.contains(&pc) || terminated) {
                if !self.dry_run {
                    if i > 0 {
                        if !terminated {
                            writeln!(out, "{indent}pc = 0x{pc:08X};").unwrap();
                        }
                        writeln!(out, "        }}").unwrap();
                    }
                    writeln!(out, "        0x{pc:08X} => {{").unwrap();
                }
            } else if terminated {
                break;
            }
            let (stmt, term) = self.insn(pc, w).unwrap_or_else(|| {
                if !self.dry_run {
                    self.stats.invalid.push((pc, w));
                }
                let fl = self.flush_stmt();
                (format!("{{ {fl}return c.invalid(0x{pc:08X}, 0x{w:08X}); }}"), true)
            });
            if !self.dry_run && !stmt.is_empty() {
                writeln!(out, "{indent}{stmt}").unwrap();
            }
            terminated = term;
        }
        if !terminated {
            if !self.dry_run {
                self.stats.fallthrough += 1;
            }
            let stmt = self.jump(end - 4, end);
            if !self.dry_run {
                writeln!(out, "{indent}{stmt}").unwrap();
            }
        }
    }
}

/// Destination of a direct branch instruction (`b`, `bc` and their link forms).
pub fn branch_target(pc: u32, w: u32) -> Option<u32> {
    let base = if w & 2 != 0 { 0 } else { pc };
    match w >> 26 {
        18 => Some(base.wrapping_add((((w & 0x03FF_FFFC) << 6) as i32 >> 6) as u32)),
        16 => Some(base.wrapping_add((w & 0xFFFC) as i16 as i32 as u32)),
        _ => None,
    }
}

/// Addresses inside the function that begin a basic block, and whether it needs a state machine.
fn leaders(f: &Func, words: &[u32], data_targets: &BTreeSet<u32>) -> (BTreeSet<u32>, bool) {
    let end = f.addr + f.size;
    let mut set = BTreeSet::from([f.addr]);
    let mut looped = false;
    for (i, &w) in words.iter().enumerate() {
        let pc = f.addr + i as u32 * 4;
        let (op, lk) = (w >> 26, w & 1 != 0);
        if let Some(t) = branch_target(pc, w) {
            if !lk && t >= f.addr && t < end {
                set.insert(t);
                looped = true;
            }
        }
        let xo = (w >> 1) & 0x3FF;
        let indirect = op == 19 && xo == 528 && !lk;
        looped |= indirect;
        if (op == 18 && !lk || op == 19 && (xo == 16 || indirect)) && pc + 4 < end {
            set.insert(pc + 4);
        }
    }
    set.extend(data_targets.range(f.addr + 4..end));
    (set, looped)
}

pub fn emit_function(
    f: &Func,
    words: &[u32],
    funcs: &BTreeMap<u32, Func>,
    data_targets: &BTreeSet<u32>,
    stats: &mut Stats,
    out: &mut String,
) {
    let (leaders, looped) = leaders(f, words, data_targets);
    stats.insns += words.len();
    stats.looped += looped as usize;

    let pipe_all = f.name.trim_start_matches('_').starts_with("GX") || f.name.contains("GameCubeAsm");

    // Pass 1: discover exact register/memory usage across all reachable instructions.
    let known = {
        let mut sink = String::new();
        let mut p1 = Emitter {
            f,
            funcs,
            looped,
            pipe_all,
            dry_run: true,
            known: RegUsage::default(),
            u: RegUsage::default(),
            stats,
        };
        p1.emit_body(words, &leaders, &mut sink);
        p1.u
    };

    writeln!(out, "// {}", f.name).unwrap();
    if f.hle.is_none() && (f.name.starts_with("__save_") || f.name.starts_with("__restore_")) {
        writeln!(out, "#[inline(always)]").unwrap();
    } else if !looped && words.len() <= 16 {
        writeln!(out, "#[inline]").unwrap();
    }
    writeln!(out, "pub fn fn_{:08X}(c: &mut Ctx) {{", f.addr).unwrap();
    if let Some(i) = f.hle {
        writeln!(out, "    if c.hle({i}) {{ return; }}").unwrap();
    }

    // Bind used guest state to local SSA variables so LLVM keeps them in ARM64 registers.
    if known.uses_mem {
        writeln!(out, "    let mem = c.mem;").unwrap();
    }
    let gpr_used = known.gpr_read | known.gpr_write;
    if gpr_used != 0 {
        out.push_str("    ");
        for k in 0..32 {
            if (gpr_used >> k) & 1 != 0 {
                write!(out, "let mut r{k} = c.r[{k}]; ").unwrap();
            }
        }
        out.push('\n');
    }
    let fpr_used = known.fpr_read | known.fpr_write;
    if fpr_used != 0 {
        out.push_str("    ");
        for k in 0..32 {
            if (fpr_used >> k) & 1 != 0 {
                write!(out, "let mut f{k} = c.f[{k}]; ").unwrap();
            }
        }
        out.push('\n');
    }
    let ps_used = known.ps_read | known.ps_write;
    if ps_used != 0 {
        out.push_str("    ");
        for k in 0..32 {
            if (ps_used >> k) & 1 != 0 {
                write!(out, "let mut ps{k} = c.ps1[{k}]; ").unwrap();
            }
        }
        out.push('\n');
    }
    let cr_used = known.cr_read | known.cr_write;
    if cr_used != 0 {
        out.push_str("    ");
        for k in 0..8 {
            if (cr_used >> k) & 1 != 0 {
                write!(out, "let mut cr{k} = c.cr[{k}]; ").unwrap();
            }
        }
        out.push('\n');
    }
    if known.ctr_read || known.ctr_write {
        writeln!(out, "    let mut ctr = c.ctr;").unwrap();
    }
    if known.ca_read || known.ca_write {
        writeln!(out, "    let mut ca = c.ca;").unwrap();
    }

    if looped {
        writeln!(out, "    let mut pc: u32 = 0x{:08X};", f.addr).unwrap();
        writeln!(out, "    loop {{ match pc {{").unwrap();
    }

    let mut e = Emitter {
        f,
        funcs,
        looped,
        pipe_all,
        dry_run: false,
        known,
        u: RegUsage::default(),
        stats,
    };
    e.emit_body(words, &leaders, out);

    if looped {
        writeln!(out, "        }}").unwrap();
        let fl = e.flush_stmt();
        writeln!(out, "        _ => {{ {fl}return c.bad_jump(0x{:08X}, pc); }}", f.addr).unwrap();
        writeln!(out, "    }} }}").unwrap();
    }
    writeln!(out, "}}\n").unwrap();
}
