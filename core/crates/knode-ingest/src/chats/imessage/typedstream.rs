//! The text of an `NSAttributedString` archived as a `typedstream`, the format of the
//! `attributedBody` column of Messages: since macOS Ventura the `text` column is often empty and
//! the text is only there.
//!
//! A typedstream starts with a version byte, the word `streamtyped` and a system version, and
//! then holds typed values: a type string (`@` an object, `+` bytes, `i` an integer...) and the
//! values it names. Objects carry their class (with its version and superclass) and then their
//! own typed values, up to an end tag. Type strings and class names are written once and then
//! referred to by number, and so are objects and classes.
//!
//! An `NSAttributedString` holds its string first, as an `NSString` (or `NSMutableString`)
//! whose first value is its UTF-8 bytes, and then the runs of attributes. Only the string is
//! wanted, so the reader stops there and never needs to understand the attributes.

/// An integer of two bytes follows.
const TAG_I16: u8 = 0x81;
/// An integer of four bytes follows.
const TAG_I32: u8 = 0x82;
/// A floating point number follows.
const TAG_FLOAT: u8 = 0x83;
/// A new type string, class or object follows.
const TAG_NEW: u8 = 0x84;
/// No object or class.
const TAG_NIL: u8 = 0x85;
/// The end of an object's values.
const TAG_END: u8 = 0x86;
/// The first number a reference to something read before is written as.
const REFERENCE_BASE: u8 = 0x92;

/// The most nested objects or classes read, so a crafted blob cannot exhaust the stack.
const MAX_DEPTH: usize = 64;

/// The text of an archived `NSAttributedString`, or `None` when `bytes` is not one this reader
/// understands. Never panics, whatever the bytes.
pub(crate) fn attributed_string_text(bytes: &[u8]) -> Option<String> {
    let mut reader = Reader {
        bytes,
        pos: 0,
        strings: Vec::new(),
        objects: Vec::new(),
        depth: 0,
    };
    reader.header()?;
    // The archive holds one root value: the attributed string.
    match reader.group()? {
        Found::Text(text) => Some(text),
        Found::Nothing => None,
    }
}

/// Something read before that can be referred to by number.
#[derive(Clone)]
enum Entry {
    /// A class: its name and its superclass, if any.
    Class {
        name: Vec<u8>,
        superclass: Option<usize>,
    },
    /// An object.
    Object,
}

enum Found {
    /// The string was found.
    Text(String),
    /// The values were read and held no string.
    Nothing,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Type strings and class names, in the order they were written.
    strings: Vec<Vec<u8>>,
    /// Objects and classes, in the order they were written.
    objects: Vec<Entry>,
    depth: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    /// `04`, `0B` `streamtyped`, and the system version.
    fn header(&mut self) -> Option<()> {
        let version = self.byte()?;
        if version != 4 {
            return None;
        }
        let len = self.length()?;
        if self.take(len)? != b"streamtyped" {
            return None;
        }
        self.integer()?;
        Some(())
    }

    /// An integer: one signed byte, or a tag and two or four little-endian bytes.
    fn integer(&mut self) -> Option<i64> {
        let first = self.byte()?;
        self.integer_after(first)
    }

    fn integer_after(&mut self, first: u8) -> Option<i64> {
        match first {
            TAG_I16 => {
                let b = self.take(2)?;
                Some(i64::from(i16::from_le_bytes([b[0], b[1]])))
            }
            TAG_I32 => {
                let b = self.take(4)?;
                Some(i64::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            }
            other => Some(i64::from(other as i8)),
        }
    }

    /// A length: an integer, where two bytes are read as unsigned.
    fn length(&mut self) -> Option<usize> {
        let first = self.byte()?;
        let value = match first {
            TAG_I16 => {
                let b = self.take(2)?;
                i64::from(u16::from_le_bytes([b[0], b[1]]))
            }
            TAG_I32 => self.integer_after(first)?,
            0x80..=0xFF => return None,
            _ => i64::from(first),
        };
        usize::try_from(value).ok()
    }

    /// The number of something read before, from a reference byte (or a tagged integer).
    fn reference(&mut self, first: u8) -> Option<usize> {
        let value = match first {
            TAG_I16 | TAG_I32 => self.integer_after(first)?,
            b if b >= REFERENCE_BASE => i64::from(b),
            _ => return None,
        };
        usize::try_from(value - i64::from(REFERENCE_BASE)).ok()
    }

    /// A shared string: new (its length and bytes) or a reference to one read before.
    fn shared_string(&mut self) -> Option<Vec<u8>> {
        let first = self.byte()?;
        if first == TAG_NEW {
            let len = self.length()?;
            let s = self.take(len)?.to_vec();
            self.strings.push(s.clone());
            return Some(s);
        }
        let index = self.reference(first)?;
        self.strings.get(index).cloned()
    }

    /// One type string and the values it names.
    fn group(&mut self) -> Option<Found> {
        let types = self.shared_string()?;
        for t in types {
            match t {
                b'@' => {
                    if let Found::Text(text) = self.object()? {
                        return Some(Found::Text(text));
                    }
                }
                b'+' => {
                    // Bytes outside a string object: read past them.
                    let len = self.length()?;
                    self.take(len)?;
                }
                b'*' | b'%' | b':' | b'#' => {
                    self.shared_string()?;
                }
                b'c' | b'C' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' => {
                    self.integer()?;
                }
                b'f' => self.float(4)?,
                b'd' => self.float(8)?,
                // Arrays, structures and unions are not needed before the string.
                _ => return None,
            }
        }
        Some(Found::Nothing)
    }

    fn float(&mut self, size: usize) -> Option<()> {
        let first = self.byte()?;
        if first == TAG_FLOAT {
            self.take(size)?;
        } else {
            self.integer_after(first)?;
        }
        Some(())
    }

    /// An object: nil, a reference to one read before, or a new one with its class and values.
    fn object(&mut self) -> Option<Found> {
        let first = self.byte()?;
        match first {
            TAG_NIL => return Some(Found::Nothing),
            TAG_NEW => {}
            other => {
                self.reference(other)?;
                return Some(Found::Nothing);
            }
        }
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        self.objects.push(Entry::Object);
        let class = self.class()?;
        let is_string = class.is_some_and(|c| self.is_string_class(c));
        let found = if is_string {
            // A string's first value is its UTF-8 bytes.
            if self.peek() == Some(TAG_END) {
                Some(Found::Text(String::new()))
            } else {
                let types = self.shared_string()?;
                if types.first() != Some(&b'+') {
                    return None;
                }
                let len = self.length()?;
                let text = self.take(len)?;
                Some(Found::Text(String::from_utf8_lossy(text).into_owned()))
            }
        } else {
            loop {
                if self.peek()? == TAG_END {
                    self.pos += 1;
                    break Some(Found::Nothing);
                }
                if let Found::Text(text) = self.group()? {
                    break Some(Found::Text(text));
                }
            }
        };
        self.depth -= 1;
        found
    }

    /// A class: nil, a reference to one read before, or a new one with its name, version and
    /// superclass. Returns its number among the objects.
    fn class(&mut self) -> Option<Option<usize>> {
        let first = self.byte()?;
        match first {
            TAG_NIL => return Some(None),
            TAG_NEW => {}
            other => {
                let index = self.reference(other)?;
                return match self.objects.get(index)? {
                    Entry::Class { .. } => Some(Some(index)),
                    Entry::Object => None,
                };
            }
        }
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let name = self.shared_string()?;
        self.integer()?;
        let index = self.objects.len();
        self.objects.push(Entry::Class {
            name: name.clone(),
            superclass: None,
        });
        let superclass = self.class()?;
        self.objects[index] = Entry::Class { name, superclass };
        self.depth -= 1;
        Some(Some(index))
    }

    /// Whether a class is `NSString` or one made from it.
    fn is_string_class(&self, mut index: usize) -> bool {
        for _ in 0..MAX_DEPTH {
            match self.objects.get(index) {
                Some(Entry::Class { name, superclass }) => {
                    if name == b"NSString" || name == b"NSMutableString" {
                        return true;
                    }
                    match superclass {
                        Some(next) => index = *next,
                        None => return false,
                    }
                }
                _ => return false,
            }
        }
        false
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes typedstreams the way Messages archives an `NSAttributedString`, for tests.
    pub(crate) struct Writer {
        out: Vec<u8>,
        strings: Vec<Vec<u8>>,
        objects: usize,
        classes: Vec<(Vec<u8>, usize)>,
    }

    impl Writer {
        pub(crate) fn new() -> Self {
            let mut out = vec![4, 11];
            out.extend_from_slice(b"streamtyped");
            out.extend_from_slice(&[TAG_I16, 0xE8, 0x03]);
            Writer {
                out,
                strings: Vec::new(),
                objects: 0,
                classes: Vec::new(),
            }
        }

        fn int(&mut self, v: i64) {
            if (-110..=127).contains(&v) {
                self.out.push(v as i8 as u8);
            } else if let Ok(v) = i16::try_from(v) {
                self.out.push(TAG_I16);
                self.out.extend_from_slice(&v.to_le_bytes());
            } else {
                self.out.push(TAG_I32);
                self.out.extend_from_slice(&(v as i32).to_le_bytes());
            }
        }

        fn len(&mut self, n: usize) {
            if n < 0x80 {
                self.out.push(n as u8);
            } else if let Ok(n) = u16::try_from(n) {
                self.out.push(TAG_I16);
                self.out.extend_from_slice(&n.to_le_bytes());
            } else {
                self.out.push(TAG_I32);
                self.out.extend_from_slice(&(n as u32).to_le_bytes());
            }
        }

        fn shared(&mut self, s: &[u8]) {
            if let Some(i) = self.strings.iter().position(|x| x == s) {
                self.out.push(REFERENCE_BASE + i as u8);
            } else {
                self.strings.push(s.to_vec());
                self.out.push(TAG_NEW);
                self.len(s.len());
                self.out.extend_from_slice(s);
            }
        }

        /// A class chain, most derived first, each with its version.
        fn class(&mut self, chain: &[(&str, i64)]) {
            let Some(((name, version), rest)) = chain.split_first() else {
                self.out.push(TAG_NIL);
                return;
            };
            if let Some((_, i)) = self.classes.iter().find(|(n, _)| n == name.as_bytes()) {
                self.out.push(REFERENCE_BASE + *i as u8);
                return;
            }
            self.out.push(TAG_NEW);
            self.shared(name.as_bytes());
            self.int(*version);
            self.classes.push((name.as_bytes().to_vec(), self.objects));
            self.objects += 1;
            self.class(rest);
        }

        fn begin(&mut self, chain: &[(&str, i64)]) {
            self.out.push(TAG_NEW);
            self.objects += 1;
            self.class(chain);
        }

        fn string(&mut self, class: &str, text: &str) {
            self.shared(b"@");
            self.begin(&[(class, 1), ("NSObject", 0)]);
            self.shared(b"+");
            self.len(text.len());
            self.out.extend_from_slice(text.as_bytes());
            self.out.push(TAG_END);
        }

        /// An attributed string, its text in an object of `class`, with one run of attributes
        /// after it, as Messages writes them.
        pub(crate) fn attributed(mut self, class: &str, text: &str) -> Vec<u8> {
            self.shared(b"@");
            self.begin(&[("NSAttributedString", 0), ("NSObject", 0)]);
            self.string(class, text);
            // One run: its index and length, then the attributes' dictionary.
            self.shared(b"iI");
            self.int(1);
            self.int(text.encode_utf16().count() as i64);
            self.shared(b"@");
            self.begin(&[("NSDictionary", 0), ("NSObject", 0)]);
            self.shared(b"i");
            self.int(1);
            self.string("NSString", "__kIMMessagePartAttributeName");
            self.shared(b"@");
            self.begin(&[("NSNumber", 0), ("NSValue", 0), ("NSObject", 0)]);
            self.shared(b"*");
            self.shared(b"q");
            self.shared(b"q");
            self.int(0);
            self.out.push(TAG_END);
            self.out.push(TAG_END);
            self.out.push(TAG_END);
            self.out
        }
    }

    /// An attributed string as Messages archives it.
    pub(crate) fn sample(text: &str) -> Vec<u8> {
        Writer::new().attributed("NSString", text)
    }

    #[test]
    fn reads_the_text_of_a_message() {
        assert_eq!(
            attributed_string_text(&sample("Dinner at eight?")).as_deref(),
            Some("Dinner at eight?")
        );
    }

    #[test]
    fn matches_the_bytes_messages_writes() {
        // The layout public descriptions of the format give for a short message.
        let bytes = sample("Hi");
        let mut expected = vec![0x04, 0x0B];
        expected.extend_from_slice(b"streamtyped");
        expected.extend_from_slice(&[0x81, 0xE8, 0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84, 0x12]);
        expected.extend_from_slice(b"NSAttributedString");
        expected.extend_from_slice(&[0x00, 0x84, 0x84, 0x08]);
        expected.extend_from_slice(b"NSObject");
        expected.extend_from_slice(&[0x00, 0x85, 0x92, 0x84, 0x84, 0x84, 0x08]);
        expected.extend_from_slice(b"NSString");
        expected.extend_from_slice(&[0x01, 0x94, 0x84, 0x01, 0x2B, 0x02, b'H', b'i', 0x86]);
        assert_eq!(&bytes[..expected.len()], &expected[..]);
        assert_eq!(attributed_string_text(&bytes).as_deref(), Some("Hi"));
    }

    #[test]
    fn reads_mutable_strings_unicode_and_long_text() {
        let text = "Café 🌮 at 8, ok? ".repeat(20);
        assert!(text.len() > 0x80);
        let bytes = Writer::new().attributed("NSMutableString", &text);
        assert_eq!(attributed_string_text(&bytes), Some(text));

        let long = "a".repeat(40_000);
        assert_eq!(attributed_string_text(&sample(&long)), Some(long));
    }

    #[test]
    fn keeps_the_object_replacement_character_for_the_caller() {
        let text = "\u{FFFC}Look";
        assert_eq!(attributed_string_text(&sample(text)).as_deref(), Some(text));
    }

    #[test]
    fn refuses_what_is_not_a_typedstream() {
        assert_eq!(attributed_string_text(b""), None);
        assert_eq!(attributed_string_text(b"bplist00"), None);
        let mut bytes = sample("Hi");
        bytes[3] = b'X';
        assert_eq!(attributed_string_text(&bytes), None);
    }

    #[test]
    fn never_panics_on_cut_or_changed_bytes() {
        let bytes = sample("A message long enough to be cut in many places");
        for end in 0..bytes.len() {
            let _ = attributed_string_text(&bytes[..end]);
        }
        for i in 0..bytes.len() {
            for value in [0x00, 0x7F, 0x81, 0x82, 0x84, 0x85, 0x86, 0x92, 0xFF] {
                let mut changed = bytes.clone();
                changed[i] = value;
                let _ = attributed_string_text(&changed);
            }
        }
    }

    #[test]
    fn deep_nesting_is_refused() {
        let mut bytes = Writer::new().out;
        bytes.extend_from_slice(&[TAG_NEW, 1, b'@']);
        for _ in 0..1000 {
            bytes.extend_from_slice(&[TAG_NEW, TAG_NEW, TAG_NEW, 1, b'X', 0]);
        }
        assert_eq!(attributed_string_text(&bytes), None);
    }
}
