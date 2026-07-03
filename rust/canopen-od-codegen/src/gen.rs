//! Rust code generation from the parsed device model.
//!
//! Emits one `Od` struct holding all OD data as typed fields (the Rust
//! counterpart of CANopenEditor's generated `OD.c`/`OD.h`) plus an
//! `ObjectDictionary` impl dispatching `(index, sub)` to those fields via
//! `match` — no unsafe, no pointers, fully checked at compile time.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use crate::model::{CanOpenDevice, OdObject, OdSubObject};

/// Codegen failure with human-readable context ("object 0x1003 sub 01: ...").
pub type Error = String;

/// Scalar type mapping between CANopen and Rust.
struct Scalar {
    rust: &'static str,
    size: usize,
    data_type: &'static str,
    signed: bool,
}

fn scalar_type(data_type: &str) -> Option<Scalar> {
    let s = |rust, size, data_type, signed| Scalar { rust, size, data_type, signed };
    match data_type {
        "BOOLEAN" => Some(s("u8", 1, "Boolean", false)),
        "UNSIGNED8" => Some(s("u8", 1, "Unsigned8", false)),
        "UNSIGNED16" => Some(s("u16", 2, "Unsigned16", false)),
        "UNSIGNED32" => Some(s("u32", 4, "Unsigned32", false)),
        "UNSIGNED64" => Some(s("u64", 8, "Unsigned64", false)),
        "INTEGER8" => Some(s("i8", 1, "Integer8", true)),
        "INTEGER16" => Some(s("i16", 2, "Integer16", true)),
        "INTEGER32" => Some(s("i32", 4, "Integer32", true)),
        "INTEGER64" => Some(s("i64", 8, "Integer64", true)),
        _ => None,
    }
}

/// A parsed integer initializer, possibly `$NODEID`-relative.
struct Init {
    base: i128,
    plus_node_id: bool,
}

fn parse_int(value: &str, ctx: &str) -> Result<Init, Error> {
    let t = value.trim();
    if t.is_empty() {
        return Ok(Init { base: 0, plus_node_id: false });
    }
    if let Some(rest) = t.strip_prefix("$NODEID") {
        let rest = rest.trim().trim_start_matches('+').trim();
        let base = if rest.is_empty() { 0 } else { parse_plain_int(rest, ctx)? };
        return Ok(Init { base, plus_node_id: true });
    }
    Ok(Init { base: parse_plain_int(t, ctx)?, plus_node_id: false })
}

fn parse_plain_int(t: &str, ctx: &str) -> Result<i128, Error> {
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => i128::from_str_radix(hex, 16),
        None => t.parse(),
    };
    parsed.map_err(|_| format!("{ctx}: cannot parse integer value {t:?}"))
}

/// Validate range and render an integer literal with type suffix.
fn int_literal(base: i128, scalar: &Scalar, ctx: &str) -> Result<String, Error> {
    let bits = scalar.size as u32 * 8;
    let in_range = if scalar.signed {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        (min..=max).contains(&base)
    } else {
        (0..(1i128 << bits)).contains(&base)
    };
    if !in_range {
        return Err(format!("{ctx}: value {base} out of range for {}", scalar.rust));
    }
    if scalar.signed {
        Ok(format!("{}{}", base, scalar.rust))
    } else {
        Ok(format!("{:#X}{}", base, scalar.rust))
    }
}

/// Render the initializer expression for a scalar field.
fn init_expr(init: &Init, scalar: &Scalar, ctx: &str) -> Result<String, Error> {
    let lit = int_literal(init.base, scalar, ctx)?;
    if init.plus_node_id {
        Ok(format!("{lit} + node_id.raw() as {}", scalar.rust))
    } else {
        Ok(lit)
    }
}

fn sdo_access(raw: &str, ctx: &str) -> Result<(&'static str, bool, bool), Error> {
    // (variant, readable, writable)
    match raw {
        "" | "ACCESS_SDO_NO" => Ok(("SdoAccess::No", false, false)),
        "ACCESS_SDO_RO" => Ok(("SdoAccess::ReadOnly", true, false)),
        "ACCESS_SDO_WO" => Ok(("SdoAccess::WriteOnly", false, true)),
        "ACCESS_SDO_RW" => Ok(("SdoAccess::ReadWrite", true, true)),
        other => Err(format!("{ctx}: unknown SDO access {other:?}")),
    }
}

fn pdo_access(raw: &str, ctx: &str) -> Result<&'static str, Error> {
    match raw {
        "" | "ACCESS_PDO_NO" => Ok("PdoAccess::No"),
        "ACCESS_PDO_R" => Ok("PdoAccess::Rpdo"),
        "ACCESS_PDO_T" => Ok("PdoAccess::Tpdo"),
        "ACCESS_PDO_TR" => Ok("PdoAccess::Both"),
        other => Err(format!("{ctx}: unknown PDO access {other:?}")),
    }
}

/// Sanitize a display name into a snake_case identifier fragment.
fn sanitize(name: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim_end_matches('_').to_string();
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

fn escape_byte_string(bytes: &[u8]) -> String {
    let mut out = String::from("b\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7E => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{b:02X}");
            }
        }
    }
    out.push('"');
    out
}

/// Everything generated for one struct field.
struct Field {
    doc: String,
    name: String,
    type_decl: String,
    init: String,
}

#[derive(Default)]
struct Output {
    fields: Vec<Field>,
    info_arms: Vec<String>,
    read_arms: Vec<String>,
    write_arms: Vec<String>,
    uses_node_id: bool,
}

/// Unique struct field name for an entry: `x{index}_{sanitized name}`.
fn field_name(index: u16, source: &str, taken: &mut HashSet<String>) -> String {
    let mut name = format!("x{index:04x}_{}", sanitize(source));
    while !taken.insert(name.clone()) {
        name.push('_');
    }
    name
}

/// Generate one scalar sub-object: field plus its three match arms.
#[allow(clippy::too_many_arguments)]
fn emit_scalar_sub(
    out: &mut Output,
    index: u16,
    sub: u8,
    sub_obj: &OdSubObject,
    scalar: &Scalar,
    field: &str,
    doc: String,
    ctx: &str,
) -> Result<(), Error> {
    let init = parse_int(sub_obj.init_value(), ctx)?;
    out.uses_node_id |= init.plus_node_id;
    out.fields.push(Field {
        doc,
        name: field.to_string(),
        type_decl: scalar.rust.to_string(),
        init: init_expr(&init, scalar, ctx)?,
    });

    let (sdo_variant, readable, writable) = sdo_access(&sub_obj.sdo, ctx)?;
    let pdo_variant = pdo_access(&sub_obj.pdo, ctx)?;
    let pat = format!("({index:#06X}, {sub:#04X})");

    out.info_arms.push(format!(
        "{pat} => Ok(EntryInfo {{ data_type: DataType::{}, sdo: {sdo_variant}, pdo: {pdo_variant}, size: {} }}),",
        scalar.data_type, scalar.size
    ));
    out.read_arms.push(if readable {
        format!("{pat} => od::read_bytes(buf, &self.{field}.to_le_bytes()),")
    } else {
        format!("{pat} => Err(OdError::WriteOnly),")
    });
    out.write_arms.push(if writable {
        let mut body = format!(
            "{pat} => {{\n                let v = {}::from_le_bytes(od::exact::<{}>(data)?);\n",
            scalar.rust, scalar.size
        );
        for (limit, cmp, err) in [
            (&sub_obj.low_limit, "<", "ValueTooLow"),
            (&sub_obj.high_limit, ">", "ValueTooHigh"),
        ] {
            if !limit.trim().is_empty() {
                let bound = parse_int(limit, ctx)?;
                if bound.plus_node_id {
                    return Err(format!("{ctx}: $NODEID in value limits is not supported"));
                }
                let lit = int_literal(bound.base, scalar, ctx)?;
                let _ = writeln!(body, "                if v {cmp} {lit} {{ return Err(OdError::{err}); }}");
            }
        }
        let _ = write!(body, "                self.{field} = v;\n                Ok(())\n            }}");
        body
    } else {
        format!("{pat} => Err(OdError::ReadOnly),")
    });
    Ok(())
}

/// Generate one string sub-object.
fn emit_string_sub(
    out: &mut Output,
    index: u16,
    sub: u8,
    sub_obj: &OdSubObject,
    field: &str,
    doc: String,
    ctx: &str,
) -> Result<(), Error> {
    let init = sub_obj.init_value().as_bytes().to_vec();
    let capacity = init.len().max(sub_obj.string_length_min as usize).max(1);
    let data_type = match sub_obj.data_type.as_str() {
        "VISIBLE_STRING" => "VisibleString",
        "OCTET_STRING" => "OctetString",
        other => return Err(format!("{ctx}: unsupported string type {other:?}")),
    };
    out.fields.push(Field {
        doc,
        name: field.to_string(),
        type_decl: format!("od::OdString<{capacity}>"),
        init: format!("od::OdString::new({})", escape_byte_string(&init)),
    });

    let (sdo_variant, readable, writable) = sdo_access(&sub_obj.sdo, ctx)?;
    let pdo_variant = pdo_access(&sub_obj.pdo, ctx)?;
    let pat = format!("({index:#06X}, {sub:#04X})");

    out.info_arms.push(format!(
        "{pat} => Ok(EntryInfo {{ data_type: DataType::{data_type}, sdo: {sdo_variant}, pdo: {pdo_variant}, size: self.{field}.len() }}),"
    ));
    out.read_arms.push(if readable {
        format!("{pat} => od::read_bytes(buf, self.{field}.as_bytes()),")
    } else {
        format!("{pat} => Err(OdError::WriteOnly),")
    });
    out.write_arms.push(if writable {
        format!("{pat} => self.{field}.set(data),")
    } else {
        format!("{pat} => Err(OdError::ReadOnly),")
    });
    Ok(())
}

/// Generate one DOMAIN sub-object: no storage; the application will attach a
/// handler via OD extensions once those are ported.
fn emit_domain_sub(
    out: &mut Output,
    index: u16,
    sub: u8,
    sub_obj: &OdSubObject,
    ctx: &str,
) -> Result<(), Error> {
    let (sdo_variant, _, _) = sdo_access(&sub_obj.sdo, ctx)?;
    let pdo_variant = pdo_access(&sub_obj.pdo, ctx)?;
    let pat = format!("({index:#06X}, {sub:#04X})");
    out.info_arms.push(format!(
        "{pat} => Ok(EntryInfo {{ data_type: DataType::Domain, sdo: {sdo_variant}, pdo: {pdo_variant}, size: 0 }}),"
    ));
    out.read_arms.push(format!("{pat} => Err(OdError::NoData),"));
    out.write_arms.push(format!("{pat} => Err(OdError::NoData),"));
    Ok(())
}

fn emit_sub(
    out: &mut Output,
    index: u16,
    sub: u8,
    obj: &OdObject,
    sub_obj: &OdSubObject,
    taken: &mut HashSet<String>,
) -> Result<(), Error> {
    let ctx = format!("object {index:#06X} sub {sub:#04X} ({})", obj.name);
    // VAR objects have one anonymous sub 00: name the field after the object.
    let source = if sub_obj.ident_source().is_empty() {
        obj.ident_source()
    } else {
        sub_obj.ident_source()
    };
    let doc = format!(
        "{index:#06X}:{sub:02X} {}{}{} — {}, {}",
        obj.name,
        if sub_obj.name.is_empty() { "" } else { " / " },
        sub_obj.name,
        sub_obj.data_type,
        sub_obj.sdo.trim_start_matches("ACCESS_SDO_").to_lowercase(),
    );
    match sub_obj.data_type.as_str() {
        "DOMAIN" => emit_domain_sub(out, index, sub, sub_obj, &ctx),
        "VISIBLE_STRING" | "OCTET_STRING" => {
            let field = field_name(index, source, taken);
            emit_string_sub(out, index, sub, sub_obj, &field, doc, &ctx)
        }
        dt => {
            let scalar = scalar_type(dt).ok_or(format!("{ctx}: unsupported data type {dt:?}"))?;
            let field = field_name(index, source, taken);
            emit_scalar_sub(out, index, sub, sub_obj, &scalar, &field, doc, &ctx)
        }
    }
}

/// Generate an ARRAY object: one count field (sub 00) and one `[T; N]` field
/// for the elements (subs 01..=N, uniform type).
fn emit_array(
    out: &mut Output,
    index: u16,
    obj: &OdObject,
    subs: &BTreeMap<u8, &OdSubObject>,
    taken: &mut HashSet<String>,
) -> Result<(), Error> {
    let ctx = format!("array object {index:#06X} ({})", obj.name);
    let count_sub = subs.get(&0).ok_or(format!("{ctx}: missing sub 00"))?;
    emit_sub(out, index, 0, obj, count_sub, taken)?;

    let n = subs.len() - 1;
    if n == 0 {
        return Ok(());
    }
    let first = subs.get(&1).ok_or(format!("{ctx}: elements must start at sub 01"))?;
    for (i, (sub, s)) in subs.iter().skip(1).enumerate() {
        if usize::from(*sub) != i + 1 {
            return Err(format!("{ctx}: element sub-indices must be contiguous"));
        }
        if s.data_type != first.data_type || s.sdo != first.sdo || s.pdo != first.pdo {
            return Err(format!("{ctx}: elements must share data type and access"));
        }
    }
    let scalar = scalar_type(&first.data_type)
        .ok_or(format!("{ctx}: unsupported element type {:?}", first.data_type))?;

    let field = field_name(index, obj.ident_source(), taken);
    let mut inits = Vec::with_capacity(n);
    for (_sub, s) in subs.iter().skip(1) {
        let init = parse_int(s.init_value(), &ctx)?;
        out.uses_node_id |= init.plus_node_id;
        inits.push(init_expr(&init, &scalar, &ctx)?);
    }
    let init = if inits.iter().all(|i| i == &inits[0]) {
        format!("[{}; {n}]", inits[0])
    } else {
        format!("[{}]", inits.join(", "))
    };
    out.fields.push(Field {
        doc: format!(
            "{index:#06X}:01..={n:02X} {} — {}[{n}], {}",
            obj.name,
            first.data_type,
            first.sdo.trim_start_matches("ACCESS_SDO_").to_lowercase(),
        ),
        name: field.clone(),
        type_decl: format!("[{}; {n}]", scalar.rust),
        init,
    });

    let (sdo_variant, readable, writable) = sdo_access(&first.sdo, &ctx)?;
    let pdo_variant = pdo_access(&first.pdo, &ctx)?;
    let pat = format!("({index:#06X}, s @ 0x01..={n:#04X})");

    out.info_arms.push(format!(
        "({index:#06X}, 0x01..={n:#04X}) => Ok(EntryInfo {{ data_type: DataType::{}, sdo: {sdo_variant}, pdo: {pdo_variant}, size: {} }}),",
        scalar.data_type, scalar.size
    ));
    out.read_arms.push(if readable {
        format!("{pat} => od::read_bytes(buf, &self.{field}[(s - 1) as usize].to_le_bytes()),")
    } else {
        format!("({index:#06X}, 0x01..={n:#04X}) => Err(OdError::WriteOnly),")
    });
    out.write_arms.push(if writable {
        let mut body = format!(
            "{pat} => {{\n                let v = {}::from_le_bytes(od::exact::<{}>(data)?);\n",
            scalar.rust, scalar.size
        );
        for (limit, cmp, err) in [
            (&first.low_limit, "<", "ValueTooLow"),
            (&first.high_limit, ">", "ValueTooHigh"),
        ] {
            if !limit.trim().is_empty() {
                let bound = parse_int(limit, &ctx)?;
                let lit = int_literal(bound.base, &scalar, &ctx)?;
                let _ = writeln!(body, "                if v {cmp} {lit} {{ return Err(OdError::{err}); }}");
            }
        }
        let _ = write!(
            body,
            "                self.{field}[(s - 1) as usize] = v;\n                Ok(())\n            }}"
        );
        body
    } else {
        format!("({index:#06X}, 0x01..={n:#04X}) => Err(OdError::ReadOnly),")
    });
    Ok(())
}

/// Generate the complete Rust module source for a device description.
pub fn generate(device: &CanOpenDevice) -> Result<String, Error> {
    let mut out = Output::default();
    let mut taken = HashSet::new();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();

    for (index_str, obj) in &device.objects {
        if obj.disabled {
            continue;
        }
        let index = u16::from_str_radix(index_str, 16)
            .map_err(|_| format!("invalid object index {index_str:?}"))?;
        if !obj.count_label.is_empty() {
            *counts.entry(obj.count_label.clone()).or_default() += 1;
        }

        let mut subs: BTreeMap<u8, &OdSubObject> = BTreeMap::new();
        for (sub_str, sub_obj) in &obj.sub_objects {
            let sub = u8::from_str_radix(sub_str, 16)
                .map_err(|_| format!("object {index_str}: invalid sub-index {sub_str:?}"))?;
            subs.insert(sub, sub_obj);
        }

        match obj.object_type.as_str() {
            "OBJECT_TYPE_VAR" => {
                let sub_obj = subs
                    .get(&0)
                    .ok_or(format!("VAR object {index_str} has no sub 00"))?;
                emit_sub(&mut out, index, 0, obj, sub_obj, &mut taken)?;
            }
            "OBJECT_TYPE_ARRAY" => emit_array(&mut out, index, obj, &subs, &mut taken)?,
            "OBJECT_TYPE_RECORD" => {
                for (sub, sub_obj) in &subs {
                    emit_sub(&mut out, index, *sub, obj, sub_obj, &mut taken)?;
                }
            }
            other => return Err(format!("object {index_str}: unknown object type {other:?}")),
        }

        // Per-object catch-all: existing object, unknown sub-index. Emitted
        // directly after the object's own arms, so it cannot shadow them.
        for arms in [&mut out.info_arms, &mut out.read_arms, &mut out.write_arms] {
            arms.push(format!("({index:#06X}, _) => Err(OdError::SubIndexNotFound),"));
        }
    }

    render(device, &out, &counts)
}

fn render(
    device: &CanOpenDevice,
    out: &Output,
    counts: &BTreeMap<String, u32>,
) -> Result<String, Error> {
    let mut w = String::new();
    let name = if device.device_info.product_name.is_empty() {
        "unnamed device"
    } else {
        &device.device_info.product_name
    };
    let _ = write!(
        w,
        "// @generated by canopen-od-codegen — DO NOT EDIT\n\
         // Device: {name}\n\
         // Source: CANopenEditor protobuf-JSON device description\n\n\
         use ::canopen_core::od::{{self, DataType, EntryInfo, ObjectDictionary, OdError, PdoAccess, SdoAccess}};\n\
         use ::canopen_core::NodeId;\n\n"
    );

    for (label, count) in counts {
        let _ = write!(
            w,
            "/// Number of enabled OD objects labelled {label} (`OD_CNT_{label}`).\npub const CNT_{label}: u8 = {count};\n"
        );
    }

    let _ = write!(
        w,
        "\n/// Object dictionary data of one device instance.\n\
         ///\n\
         /// Fields are plain typed data for direct application access; SDO/PDO\n\
         /// access goes through the [`ObjectDictionary`] impl.\n\
         #[derive(Debug, Clone)]\n\
         #[allow(clippy::struct_field_names)]\n\
         pub struct Od {{\n"
    );
    for f in &out.fields {
        let _ = write!(w, "    /// {}\n    pub {}: {},\n", f.doc, f.name, f.type_decl);
    }
    w.push_str("}\n\n");

    let node_id_param = if out.uses_node_id { "node_id" } else { "_node_id" };
    let _ = write!(
        w,
        "impl Od {{\n    /// OD initialized with the configured default/actual values;\n    /// `$NODEID`-relative COB-IDs are computed from `{node_id_param}`.\n    #[allow(clippy::unreadable_literal)]\n    pub fn new({node_id_param}: NodeId) -> Self {{\n        Self {{\n"
    );
    for f in &out.fields {
        let _ = writeln!(w, "            {}: {},", f.name, f.init);
    }
    w.push_str("        }\n    }\n}\n\n");

    let _ = write!(
        w,
        "#[allow(clippy::unreadable_literal, clippy::match_same_arms, clippy::too_many_lines)]\n\
         impl ObjectDictionary for Od {{\n"
    );
    for (sig, arms) in [
        (
            "fn info(&self, index: u16, sub: u8) -> Result<EntryInfo, OdError>",
            &out.info_arms,
        ),
        (
            "fn read(&self, index: u16, sub: u8, buf: &mut [u8]) -> Result<usize, OdError>",
            &out.read_arms,
        ),
        (
            "fn write(&mut self, index: u16, sub: u8, data: &[u8]) -> Result<(), OdError>",
            &out.write_arms,
        ),
    ] {
        let _ = write!(w, "    {sig} {{\n        match (index, sub) {{\n");
        for arm in arms {
            let _ = writeln!(w, "            {arm}");
        }
        w.push_str("            _ => Err(OdError::ObjectNotFound),\n        }\n    }\n\n");
    }
    w.push_str("}\n");
    Ok(w)
}
