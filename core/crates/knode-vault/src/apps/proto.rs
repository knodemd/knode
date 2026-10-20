//! Just enough of the protobuf wire format to read Apple Notes bodies: fields by number, with
//! varints and length-delimited values. Unknown fields are skipped.

/// The value of one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

/// One message: its fields in the order they were written.
#[derive(Debug, Clone, Default)]
pub(crate) struct Message<'a> {
    fields: Vec<(u32, Value<'a>)>,
}

fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

impl<'a> Message<'a> {
    /// Reads a message; `None` when the bytes are not one.
    pub(crate) fn parse(bytes: &'a [u8]) -> Option<Message<'a>> {
        let mut fields = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let key = varint(bytes, &mut at)?;
            let number = u32::try_from(key >> 3).ok()?;
            let value = match key & 7 {
                0 => Value::Varint(varint(bytes, &mut at)?),
                1 => {
                    let raw = bytes.get(at..at + 8)?;
                    at += 8;
                    Value::Fixed64(u64::from_le_bytes(raw.try_into().ok()?))
                }
                2 => {
                    let len = usize::try_from(varint(bytes, &mut at)?).ok()?;
                    let raw = bytes.get(at..at.checked_add(len)?)?;
                    at += len;
                    Value::Bytes(raw)
                }
                5 => {
                    let raw = bytes.get(at..at + 4)?;
                    at += 4;
                    Value::Fixed32(u32::from_le_bytes(raw.try_into().ok()?))
                }
                _ => return None,
            };
            if number == 0 {
                return None;
            }
            fields.push((number, value));
        }
        Some(Message { fields })
    }

    /// The last value of a field, as protobuf reads repeated scalars.
    fn get(&self, number: u32) -> Option<Value<'a>> {
        self.fields
            .iter()
            .rev()
            .find(|(n, _)| *n == number)
            .map(|(_, v)| *v)
    }

    /// A varint field.
    pub(crate) fn int(&self, number: u32) -> Option<u64> {
        match self.get(number)? {
            Value::Varint(v) => Some(v),
            _ => None,
        }
    }

    /// A length-delimited field.
    pub(crate) fn bytes(&self, number: u32) -> Option<&'a [u8]> {
        match self.get(number)? {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// A string field.
    pub(crate) fn string(&self, number: u32) -> Option<String> {
        self.bytes(number)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// An embedded message field.
    pub(crate) fn message(&self, number: u32) -> Option<Message<'a>> {
        Message::parse(self.bytes(number)?)
    }

    /// Every embedded message of a repeated field.
    pub(crate) fn messages(&self, number: u32) -> Vec<Message<'a>> {
        self.fields
            .iter()
            .filter(|(n, _)| *n == number)
            .filter_map(|(_, v)| match v {
                Value::Bytes(b) => Message::parse(b),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields() {
        // field 1 varint 150, field 2 string "hi", field 3 message { field 1 varint 1 }.
        let bytes = [
            0x08, 0x96, 0x01, 0x12, 0x02, b'h', b'i', 0x1a, 0x02, 0x08, 0x01,
        ];
        let m = Message::parse(&bytes).unwrap();
        assert_eq!(m.int(1), Some(150));
        assert_eq!(m.string(2).as_deref(), Some("hi"));
        assert_eq!(m.message(3).unwrap().int(1), Some(1));
        assert!(Message::parse(&[0x12, 0x05, b'h']).is_none());
    }
}
