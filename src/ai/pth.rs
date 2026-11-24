//! Reading PyTorch `.pth` weight files without PyTorch.
//!
//! A `.pth` file is an uncompressed zip holding `data.pkl`, a pickle that
//! names every tensor and points at its raw storage, and one `data/<key>`
//! entry of little-endian floats per storage. The pickle is read by a small
//! interpreter that understands only what `torch.save` writes for a plain
//! state dict and builds no objects: tensors become (storage key, offset,
//! shape) and every other class is opaque. Nothing in the file is executed.

use std::collections::HashMap;

/// A named tensor's place in the archive.
struct Tensor {
    key: String,
    offset: usize,
    shape: Vec<usize>,
}

/// All tensors of a state dict, by name.
pub(super) struct Weights<'a> {
    file: &'a str,
    tensors: HashMap<String, Tensor>,
    storages: HashMap<String, &'a [u8]>,
}

impl<'a> Weights<'a> {
    pub fn parse(file: &'a str, bytes: &'a [u8]) -> Result<Self, String> {
        let entries = zip_entries(bytes)?;
        let (prefix, pkl) = entries
            .iter()
            .find_map(|(n, d)| n.strip_suffix("data.pkl").map(|p| (p.to_string(), *d)))
            .ok_or_else(|| format!("{file}: not a PyTorch weights file"))?;
        let root = unpickle(pkl).map_err(|e| format!("{file}: {e}"))?;
        let dict = match root {
            V::Dict(items) => {
                let inner = items.iter().find_map(|(k, v)| match (k, v) {
                    (V::Str(k), V::Dict(_)) if k == "params_ema" || k == "params" => Some(v),
                    _ => None,
                });
                match inner {
                    Some(V::Dict(i)) => i.clone(),
                    _ => items,
                }
            }
            _ => return Err(format!("{file}: unexpected contents")),
        };
        let mut tensors = HashMap::new();
        for (k, v) in dict {
            if let (V::Str(name), V::Tensor(t)) = (k, v) {
                tensors.insert(name, t);
            }
        }
        let storages = entries
            .into_iter()
            .filter_map(|(n, d)| {
                n.strip_prefix(&prefix)
                    .and_then(|r| r.strip_prefix("data/"))
                    .map(|k| (k.to_string(), d))
            })
            .collect();
        Ok(Weights {
            file,
            tensors,
            storages,
        })
    }

    pub fn has(&self, name: &str) -> bool {
        self.tensors.contains_key(name)
    }

    pub fn shape(&self, name: &str) -> Result<&[usize], String> {
        self.tensors
            .get(name)
            .map(|t| t.shape.as_slice())
            .ok_or_else(|| format!("{}: {name} missing", self.file))
    }

    /// The tensor's values; fails unless it has exactly `shape`.
    pub fn get(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>, String> {
        let t = self
            .tensors
            .get(name)
            .ok_or_else(|| format!("{}: {name} missing", self.file))?;
        if t.shape != shape {
            return Err(format!(
                "{}: {name} is {:?}, expected {shape:?}",
                self.file, t.shape
            ));
        }
        let n: usize = shape.iter().product();
        let data = self
            .storages
            .get(&t.key)
            .and_then(|d| d.get(t.offset * 4..(t.offset + n) * 4))
            .ok_or_else(|| format!("{}: data of {name} missing", self.file))?;
        Ok(data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }
}

// ----- zip --------------------------------------------------------------------

fn u16_at(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
        .ok_or_else(|| "model file is truncated".to_string())
}

fn u32_at(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| "model file is truncated".to_string())
}

/// The entries of an uncompressed zip, by name.
fn zip_entries(b: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    let bad = || "model file is not a PyTorch archive".to_string();
    let eocd = (0..b.len().saturating_sub(21))
        .rev()
        .take(70_000)
        .find(|&i| b[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(bad)?;
    let count = u16_at(b, eocd + 10)?;
    let mut p = u32_at(b, eocd + 16)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if b.get(p..p + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(bad());
        }
        let method = u16_at(b, p + 10)?;
        let size = u32_at(b, p + 20)?;
        let name_len = u16_at(b, p + 28)?;
        let extra_len = u16_at(b, p + 30)?;
        let comment_len = u16_at(b, p + 32)?;
        let local = u32_at(b, p + 42)?;
        let name = b.get(p + 46..p + 46 + name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).into_owned();
        p += 46 + name_len + extra_len + comment_len;
        if method != 0 {
            return Err(bad());
        }
        if b.get(local..local + 4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
            return Err(bad());
        }
        let start = local + 30 + u16_at(b, local + 26)? + u16_at(b, local + 28)?;
        let data = b.get(start..start + size).ok_or_else(bad)?;
        out.push((name, data));
    }
    Ok(out)
}

// ----- pickle -----------------------------------------------------------------

#[derive(Clone)]
enum V {
    Mark,
    None,
    Int(i64),
    Float,
    Str(String),
    Tuple(Vec<V>),
    List(Vec<V>),
    Dict(Vec<(V, V)>),
    Global(String),
    Persistent(Vec<V>),
    Tensor(Tensor),
    Opaque,
}

impl Clone for Tensor {
    fn clone(&self) -> Self {
        Tensor {
            key: self.key.clone(),
            offset: self.offset,
            shape: self.shape.clone(),
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self
            .b
            .get(self.p..self.p + n)
            .ok_or("pickle is truncated")?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<usize, String> {
        Ok(self.take(1)?[0] as usize)
    }
    fn u16(&mut self) -> Result<usize, String> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]) as usize)
    }
    fn u32(&mut self) -> Result<usize, String> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
    }
    fn line(&mut self) -> Result<String, String> {
        let rest = self.b.get(self.p..).ok_or("pickle is truncated")?;
        let n = rest
            .iter()
            .position(|&c| c == b'\n')
            .ok_or("pickle is truncated")?;
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.p += n + 1;
        Ok(s)
    }
    fn text(&mut self, n: usize) -> Result<V, String> {
        Ok(V::Str(String::from_utf8_lossy(self.take(n)?).into_owned()))
    }
}

fn pop(stack: &mut Vec<V>) -> Result<V, String> {
    stack
        .pop()
        .ok_or_else(|| "pickle stack underflow".to_string())
}

fn pop_mark(stack: &mut Vec<V>) -> Result<Vec<V>, String> {
    let at = stack
        .iter()
        .rposition(|v| matches!(v, V::Mark))
        .ok_or("pickle mark missing")?;
    let items = stack.split_off(at + 1);
    stack.pop();
    Ok(items)
}

fn as_int(v: &V) -> Option<usize> {
    match v {
        V::Int(i) if *i >= 0 => Some(*i as usize),
        _ => None,
    }
}

/// What `callable(*args)` stands for. Only tensors and ordered dicts matter.
fn reduce(callable: V, args: V) -> V {
    let (V::Global(name), V::Tuple(args)) = (callable, args) else {
        return V::Opaque;
    };
    match name.as_str() {
        "collections OrderedDict" => V::Dict(Vec::new()),
        "torch._utils _rebuild_tensor_v2" => {
            let (Some(V::Persistent(pid)), Some(off), Some(V::Tuple(shape))) =
                (args.first(), args.get(1).and_then(as_int), args.get(2))
            else {
                return V::Opaque;
            };
            let Some(V::Str(key)) = pid.get(2) else {
                return V::Opaque;
            };
            let shape: Option<Vec<usize>> = shape.iter().map(as_int).collect();
            match shape {
                Some(shape) => V::Tensor(Tensor {
                    key: key.clone(),
                    offset: off,
                    shape,
                }),
                None => V::Opaque,
            }
        }
        _ => V::Opaque,
    }
}

fn unpickle(b: &[u8]) -> Result<V, String> {
    let mut r = Reader { b, p: 0 };
    let mut stack: Vec<V> = Vec::new();
    let mut memo: HashMap<usize, V> = HashMap::new();
    loop {
        let op = r.take(1)?[0];
        match op {
            0x80 => {
                r.u8()?;
            }
            0x95 => {
                r.take(8)?;
            }
            b'.' => return pop(&mut stack),
            b'(' => stack.push(V::Mark),
            b'N' => stack.push(V::None),
            0x88 => stack.push(V::Int(1)),
            0x89 => stack.push(V::Int(0)),
            b'K' => {
                let v = r.u8()?;
                stack.push(V::Int(v as i64));
            }
            b'M' => {
                let v = r.u16()?;
                stack.push(V::Int(v as i64));
            }
            b'J' => {
                let v = r.u32()? as u32 as i32;
                stack.push(V::Int(v as i64));
            }
            0x8a => {
                let n = r.u8()?;
                let s = r.take(n)?;
                let mut v: i64 = 0;
                for (i, &byte) in s.iter().enumerate().take(8) {
                    v |= (byte as i64) << (8 * i);
                }
                if n > 0 && n < 8 && s[n - 1] & 0x80 != 0 {
                    v -= 1i64 << (8 * n);
                }
                stack.push(V::Int(v));
            }
            b'G' => {
                r.take(8)?;
                stack.push(V::Float);
            }
            b'X' => {
                let n = r.u32()?;
                let v = r.text(n)?;
                stack.push(v);
            }
            0x8c => {
                let n = r.u8()?;
                let v = r.text(n)?;
                stack.push(v);
            }
            b'c' => {
                let module = r.line()?;
                let name = r.line()?;
                stack.push(V::Global(format!("{module} {name}")));
            }
            b'}' => stack.push(V::Dict(Vec::new())),
            b']' => stack.push(V::List(Vec::new())),
            b')' => stack.push(V::Tuple(Vec::new())),
            b't' => {
                let items = pop_mark(&mut stack)?;
                stack.push(V::Tuple(items));
            }
            0x85..=0x87 => {
                let n = (op - 0x84) as usize;
                if stack.len() < n {
                    return Err("pickle stack underflow".into());
                }
                let items = stack.split_off(stack.len() - n);
                stack.push(V::Tuple(items));
            }
            b'q' => {
                let i = r.u8()?;
                memo.insert(i, stack.last().cloned().ok_or("pickle stack underflow")?);
            }
            b'r' => {
                let i = r.u32()?;
                memo.insert(i, stack.last().cloned().ok_or("pickle stack underflow")?);
            }
            0x94 => {
                let i = memo.len();
                memo.insert(i, stack.last().cloned().ok_or("pickle stack underflow")?);
            }
            b'h' | b'j' => {
                let i = if op == b'h' { r.u8()? } else { r.u32()? };
                stack.push(memo.get(&i).cloned().ok_or("pickle memo missing")?);
            }
            b'Q' => {
                let pid = pop(&mut stack)?;
                stack.push(match pid {
                    V::Tuple(t) => V::Persistent(t),
                    _ => V::Opaque,
                });
            }
            b'R' => {
                let args = pop(&mut stack)?;
                let callable = pop(&mut stack)?;
                stack.push(reduce(callable, args));
            }
            b'b' => {
                pop(&mut stack)?;
            }
            b's' => {
                let v = pop(&mut stack)?;
                let k = pop(&mut stack)?;
                if let Some(V::Dict(d)) = stack.last_mut() {
                    d.push((k, v));
                }
            }
            b'u' => {
                let items = pop_mark(&mut stack)?;
                if let Some(V::Dict(d)) = stack.last_mut() {
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        d.push((k, v));
                    }
                }
            }
            b'a' => {
                let v = pop(&mut stack)?;
                if let Some(V::List(l)) = stack.last_mut() {
                    l.push(v);
                }
            }
            b'e' => {
                let items = pop_mark(&mut stack)?;
                if let Some(V::List(l)) = stack.last_mut() {
                    l.extend(items);
                }
            }
            _ => return Err(format!("unsupported pickle operation 0x{op:02x}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pickle as `torch.save({"params": OrderedDict(w=tensor)})` writes it.
    #[test]
    fn reads_a_torch_state_dict() {
        let mut p = vec![0x80, 2, b'}', b'q', 0];
        let s = |p: &mut Vec<u8>, t: &str| {
            p.push(b'X');
            p.extend_from_slice(&(t.len() as u32).to_le_bytes());
            p.extend_from_slice(t.as_bytes());
        };
        s(&mut p, "params");
        p.extend_from_slice(b"ccollections\nOrderedDict\nq\x01)R(");
        s(&mut p, "w");
        p.extend_from_slice(b"ctorch._utils\n_rebuild_tensor_v2\n((");
        s(&mut p, "storage");
        p.extend_from_slice(b"ctorch\nFloatStorage\n");
        s(&mut p, "7");
        s(&mut p, "cpu");
        p.extend_from_slice(b"K\x06tQK\x02K\x02K\x02\x86K\x02K\x01\x86\x89h\x01)Rtq\x02Ru");
        p.extend_from_slice(b"s.");
        let V::Dict(top) = unpickle(&p).unwrap() else {
            panic!("not a dict")
        };
        let V::Dict(inner) = &top[0].1 else {
            panic!("no inner dict")
        };
        let V::Tensor(t) = &inner[0].1 else {
            panic!("no tensor")
        };
        assert_eq!(
            (t.key.as_str(), t.offset, t.shape.as_slice()),
            ("7", 2, &[2, 2][..])
        );
    }

    #[test]
    fn refuses_unknown_operations() {
        // Calls to anything but the tensor and dict builders stay inert.
        let call = unpickle(b"\x80\x02cos\nsystem\n(X\x02\x00\x00\x00lstR.").unwrap();
        assert!(matches!(call, V::Opaque));
        assert!(unpickle(b"\x80\x02\xff.").is_err());
    }
}
