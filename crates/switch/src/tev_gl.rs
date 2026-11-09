//! GameCube Texture Environment (TEV) to OpenGL 4.3 Core (`#version 430 core`) fragment shader generator.
//! Supports up to 16 TEV stages, compare modes, dual-reference alpha test, and perspective/ortho fog.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::hash::{Hash, Hasher};

#[derive(Clone, Copy, Hash)]
pub struct Stage {
    color: u32,
    alpha: u32,
    order: u32,
    ksel: u32,
}

#[derive(Hash)]
pub struct Config {
    stages: Vec<Stage>,
    alpha_test: u32,
    fog: u32,
}

struct KeyHasher(u64);

impl Hasher for KeyHasher {
    #[inline]
    fn finish(&self) -> u64 {
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        h ^ (h >> 33)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

const UV: [&str; 8] = [
    "v_uv01.xy", "v_uv01.zw", "v_uv23.xy", "v_uv23.zw",
    "v_uv45.xy", "v_uv45.zw", "v_uv67.xy", "v_uv67.zw",
];
const REGISTERS: [&str; 4] = ["prev", "r0", "r1", "r2"];
const FRACTIONS: [&str; 8] = ["1.0", "0.875", "0.75", "0.625", "0.5", "0.375", "0.25", "0.125"];

fn color_input(code: u32) -> &'static str {
    [
        "prev.rgb", "vec3(prev.a)", "r0.rgb", "vec3(r0.a)", "r1.rgb", "vec3(r1.a)", "r2.rgb",
        "vec3(r2.a)", "tex.rgb", "vec3(tex.a)", "ras.rgb", "vec3(ras.a)", "vec3(1.0)",
        "vec3(0.5)", "kc", "vec3(0.0)",
    ][code as usize & 15]
}

fn alpha_input(code: u32) -> &'static str {
    ["prev.a", "r0.a", "r1.a", "r2.a", "tex.a", "ras.a", "ka", "0.0"][code as usize & 7]
}

fn konst_color(sel: u32) -> String {
    match sel {
        0..=7 => format!("vec3({})", FRACTIONS[sel as usize]),
        12..=15 => format!("p.konst[{}].rgb", sel - 12),
        16..=31 => format!("vec3(p.konst[{}].{})", sel & 3, ["r", "g", "b", "a"][(sel as usize - 16) / 4]),
        _ => "vec3(0.0)".into(),
    }
}

fn konst_alpha(sel: u32) -> String {
    match sel {
        0..=7 => FRACTIONS[sel as usize].into(),
        16..=31 => format!("p.konst[{}].{}", sel & 3, ["r", "g", "b", "a"][(sel as usize - 16) / 4]),
        _ => "0.0".into(),
    }
}

fn alpha_compare(code: u32, reference: &str) -> String {
    match code & 7 {
        0 => "false".into(),
        7 => "true".into(),
        c => format!("(a8 {} {reference})", ["", "<", "==", "<=", ">", "!=", ">="][c as usize]),
    }
}

impl Config {
    pub fn from_registers(bp: &[u32; 256]) -> Config {
        let count = ((bp[0x00] >> 10) & 15) as usize + 1;
        let stages = (0..count)
            .map(|i| {
                let shift = 12 * (i % 2) as u32;
                Stage {
                    color: bp[0xC0 + i * 2] & 0xFF_FFFF,
                    alpha: bp[0xC1 + i * 2] & 0xFF_FFF0,
                    order: (bp[0x28 + i / 2] >> shift) & 0x3FF,
                    ksel: (bp[0xF6 + i / 2] >> if i % 2 == 0 { 4 } else { 14 }) & 0x3FF,
                }
            })
            .collect();
        let fog = ((bp[0xF1] >> 21) & 7) | ((bp[0xF1] >> 20) & 1) << 3;
        Config { stages, alpha_test: (bp[0xF3] >> 16) & 0xFF, fog }
    }

    pub fn passthrough() -> Config {
        Config {
            stages: vec![Stage { color: 0x08_FFFA, alpha: 0x08_FFD0, order: 0, ksel: 0 }],
            alpha_test: 0x3F,
            fog: 0,
        }
    }

    pub fn key(&self) -> u64 {
        let mut hasher = KeyHasher(0xCBF2_9CE4_8422_2325);
        self.hash(&mut hasher);
        hasher.finish()
    }

    pub fn texture_uses(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.stages
            .iter()
            .filter(|s| s.order & 0x40 != 0)
            .map(|s| ((s.order & 7) as usize, ((s.order >> 3) & 7) as usize))
    }

    pub fn fragment_glsl(&self) -> String {
        let mut s = String::with_capacity(2048);
        s.push_str(
            "#version 430 core\n\
             layout(std140, binding = 0) uniform Params {\n\
                 vec4 reg[4];\n\
                 vec4 konst[4];\n\
                 vec4 alpha_ref;\n\
                 vec4 fog_color;\n\
                 vec4 fog;\n\
             } p;\n\
             layout(binding = 0) uniform sampler2D t0;\n\
             layout(binding = 1) uniform sampler2D t1;\n\
             layout(binding = 2) uniform sampler2D t2;\n\
             layout(binding = 3) uniform sampler2D t3;\n\
             layout(binding = 4) uniform sampler2D t4;\n\
             layout(binding = 5) uniform sampler2D t5;\n\
             layout(binding = 6) uniform sampler2D t6;\n\
             layout(binding = 7) uniform sampler2D t7;\n\
             layout(location = 0) in vec4 v_c0;\n\
             layout(location = 1) in vec4 v_c1;\n\
             layout(location = 2) in vec4 v_uv01;\n\
             layout(location = 3) in vec4 v_uv23;\n\
             layout(location = 4) in vec4 v_uv45;\n\
             layout(location = 5) in vec4 v_uv67;\n\
             layout(location = 0) out vec4 f_out;\n\
             void main() {\n\
                 vec4 prev = p.reg[0];\n\
                 vec4 r0 = p.reg[1];\n\
                 vec4 r1 = p.reg[2];\n\
                 vec4 r2 = p.reg[3];\n",
        );

        for stage in &self.stages {
            let (map, coord, channel) = (stage.order & 7, (stage.order >> 3) & 7, (stage.order >> 7) & 7);
            let tex = match stage.order & 0x40 {
                0 => "vec4(1.0)".to_string(),
                _ => format!("texture(t{map}, {})", UV[coord as usize]),
            };
            let ras = ["v_c0", "v_c1"].get(channel as usize).copied().unwrap_or("vec4(0.0)");
            writeln!(s, "    {{\n        vec4 tex = {tex};\n        vec4 ras = {ras};").unwrap();
            writeln!(
                s,
                "        vec3 kc = {};\n        float ka = {};",
                konst_color(stage.ksel & 31),
                konst_alpha((stage.ksel >> 5) & 31)
            )
            .unwrap();

            let (c, a) = (stage.color, stage.alpha);
            let ci = |shift: u32| color_input(c >> shift);
            let ai = |shift: u32| alpha_input(a >> shift);
            writeln!(s, "        vec3 ca = {}; vec3 cb = {}; vec3 cc = {}; vec3 cd = {};", ci(12), ci(8), ci(4), ci(0)).unwrap();
            writeln!(s, "        float aa = {}; float ab = {}; float ac = {}; float ad = {};", ai(13), ai(10), ai(7), ai(4)).unwrap();
            writeln!(s, "        vec3 col = {};", combine(c, true)).unwrap();
            writeln!(s, "        float alp = {};", combine(a, false)).unwrap();

            let limits = |env: u32| if env & (1 << 19) != 0 { ("0.0", "1.0") } else { ("-4.0", "4.0") };
            let ((clo, chi), (alo, ahi)) = (limits(c), limits(a));
            let (cdest, adest) = (REGISTERS[((c >> 22) & 3) as usize], REGISTERS[((a >> 22) & 3) as usize]);
            writeln!(s, "        {cdest} = vec4(clamp(col, vec3({clo}), vec3({chi})), {cdest}.a);").unwrap();
            writeln!(s, "        {adest}.a = clamp(alp, {alo}, {ahi});\n    }}").unwrap();
        }

        let (comp0, comp1, logic) = (self.alpha_test & 7, (self.alpha_test >> 3) & 7, (self.alpha_test >> 6) & 3);
        if (comp0, comp1) != (7, 7) || logic >= 2 {
            let (first, second) = (alpha_compare(comp0, "p.alpha_ref.x"), alpha_compare(comp1, "p.alpha_ref.y"));
            let op = ["&&", "||", "!=", "=="][logic as usize];
            writeln!(s, "    float a8 = floor(clamp(prev.a, 0.0, 1.0) * 255.0 + 0.5);").unwrap();
            writeln!(s, "    if (!({first} {op} {second})) {{ discard; }}").unwrap();
        }
        s.push_str("    prev = clamp(prev, vec4(0.0), vec4(1.0));\n");
        if self.fog & 7 != 0 {
            s.push_str("    float z24 = floor(clamp(gl_FragCoord.z, 0.0, 1.0) * 16777215.0);\n");
            s.push_str(if self.fog & 8 == 0 {
                "    float eye = p.fog.x * 16777216.0 / (p.fog.y - floor(z24 / p.fog.z));\n"
            } else {
                "    float eye = p.fog.x * z24 / 16777216.0;\n"
            });
            s.push_str("    float fog_t = clamp(eye - p.fog.w, 0.0, 1.0);\n");
            let amount = match self.fog & 7 {
                4 => "1.0 - exp2(-8.0 * fog_t)",
                5 => "1.0 - exp2(-8.0 * fog_t * fog_t)",
                6 => "exp2(-8.0 * (1.0 - fog_t))",
                7 => "exp2(-8.0 * (1.0 - fog_t) * (1.0 - fog_t))",
                _ => "fog_t",
            };
            writeln!(s, "    prev = vec4(mix(prev.rgb, p.fog_color.rgb, {amount}), prev.a);").unwrap();
        }
        s.push_str("    f_out = prev;\n}\n");
        s
    }
}

fn combine(env: u32, color: bool) -> String {
    let (bias, subtract, scale) = ((env >> 16) & 3, env & (1 << 18) != 0, (env >> 20) & 3);
    let (a, b, c, d) = if color { ("ca", "cb", "cc", "cd") } else { ("aa", "ab", "ac", "ad") };
    if bias != 3 {
        let op = if subtract { "-" } else { "+" };
        let bias = ["", " + 0.5", " - 0.5"][bias as usize];
        let scale = ["1.0", "2.0", "4.0", "0.5"][scale as usize];
        return format!("({d} {op} mix({a}, {b}, {c}){bias}) * {scale}");
    }
    let zero = if color { "vec3(0.0)" } else { "0.0" };
    match (scale, color) {
        (3, true) if subtract => format!("{d} + vec3(lessThan(abs(ca - cb), vec3(0.002))) * {c}"),
        (3, true) => format!("{d} + vec3(greaterThan(ca, cb)) * {c}"),
        (3, false) if subtract => format!("{d} + ((abs(aa - ab) < 0.002) ? {c} : 0.0)"),
        (3, false) => format!("{d} + ((aa > ab) ? {c} : 0.0)"),
        _ => {
            let weights = ["vec3(1.0, 0.0, 0.0)", "vec3(1.0, 256.0, 0.0)", "vec3(1.0, 256.0, 65536.0)"][scale as usize];
            let value = |v: &str| format!("dot(floor({v} * 255.0 + 0.5), {weights})");
            let cond = format!("({} {} {})", value("ca"), if subtract { "==" } else { ">" }, value("cb"));
            format!("{d} + ({cond} ? {c} : {zero})")
        }
    }
}
