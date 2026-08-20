// SPDX-License-Identifier: MIT OR Apache-2.0

use super::entities::longest_named_reference;
use super::error::{HtmlError, HtmlStage};
use super::limits::{Budget, HtmlLimits, OMITTED_SCAN_BUFFER_BYTES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tag {
    Html,
    Head,
    Body,
    Title,
    Meta,
    Main,
    Article,
    Section,
    Nav,
    Aside,
    Header,
    Footer,
    Address,
    Blockquote,
    Div,
    P,
    Pre,
    Hr,
    Br,
    H(u8),
    Ul,
    Ol,
    Li,
    Dl,
    Dt,
    Dd,
    Figure,
    Figcaption,
    Details,
    Dialog,
    Fieldset,
    Hgroup,
    Search,
    Summary,
    Menu,
    Table,
    Caption,
    Colgroup,
    Col,
    Thead,
    Tbody,
    Tfoot,
    Tr,
    Td,
    Th,
    A,
    Img,
    Area,
    Noscript,
    Form,
    Input,
    Button,
    Select,
    Option,
    Textarea,
    Script,
    Style,
    Template,
    Svg,
    Math,
    Iframe,
    Object,
    Embed,
    Base,
    Link,
    B,
    Big,
    Code,
    Em,
    Font,
    I,
    S,
    Small,
    Strike,
    Strong,
    Tt,
    U,
    Nobr,
    Span,
    Other(u64),
}

impl Tag {
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"html" => Self::Html,
            b"head" => Self::Head,
            b"body" => Self::Body,
            b"title" => Self::Title,
            b"meta" => Self::Meta,
            b"main" => Self::Main,
            b"article" => Self::Article,
            b"section" => Self::Section,
            b"nav" => Self::Nav,
            b"aside" => Self::Aside,
            b"header" => Self::Header,
            b"footer" => Self::Footer,
            b"address" => Self::Address,
            b"blockquote" => Self::Blockquote,
            b"div" => Self::Div,
            b"p" => Self::P,
            b"pre" => Self::Pre,
            b"hr" => Self::Hr,
            b"br" => Self::Br,
            b"h1" => Self::H(1),
            b"h2" => Self::H(2),
            b"h3" => Self::H(3),
            b"h4" => Self::H(4),
            b"h5" => Self::H(5),
            b"h6" => Self::H(6),
            b"ul" => Self::Ul,
            b"ol" => Self::Ol,
            b"li" => Self::Li,
            b"dl" => Self::Dl,
            b"dt" => Self::Dt,
            b"dd" => Self::Dd,
            b"figure" => Self::Figure,
            b"figcaption" => Self::Figcaption,
            b"details" => Self::Details,
            b"dialog" => Self::Dialog,
            b"fieldset" => Self::Fieldset,
            b"hgroup" => Self::Hgroup,
            b"search" => Self::Search,
            b"summary" => Self::Summary,
            b"menu" => Self::Menu,
            b"table" => Self::Table,
            b"caption" => Self::Caption,
            b"colgroup" => Self::Colgroup,
            b"col" => Self::Col,
            b"thead" => Self::Thead,
            b"tbody" => Self::Tbody,
            b"tfoot" => Self::Tfoot,
            b"tr" => Self::Tr,
            b"td" => Self::Td,
            b"th" => Self::Th,
            b"a" => Self::A,
            b"img" => Self::Img,
            b"area" => Self::Area,
            b"noscript" => Self::Noscript,
            b"form" => Self::Form,
            b"input" => Self::Input,
            b"button" => Self::Button,
            b"select" => Self::Select,
            b"option" => Self::Option,
            b"textarea" => Self::Textarea,
            b"script" => Self::Script,
            b"style" => Self::Style,
            b"template" => Self::Template,
            b"svg" => Self::Svg,
            b"math" => Self::Math,
            b"iframe" => Self::Iframe,
            b"object" => Self::Object,
            b"embed" => Self::Embed,
            b"base" => Self::Base,
            b"link" => Self::Link,
            b"b" => Self::B,
            b"big" => Self::Big,
            b"code" => Self::Code,
            b"em" => Self::Em,
            b"font" => Self::Font,
            b"i" => Self::I,
            b"s" => Self::S,
            b"small" => Self::Small,
            b"strike" => Self::Strike,
            b"strong" => Self::Strong,
            b"tt" => Self::Tt,
            b"u" => Self::U,
            b"nobr" => Self::Nobr,
            b"span" => Self::Span,
            _ => Self::Other(stable_name_hash(name)),
        }
    }

    pub const fn is_void(self) -> bool {
        matches!(
            self,
            Self::Area
                | Self::Base
                | Self::Br
                | Self::Col
                | Self::Embed
                | Self::Hr
                | Self::Img
                | Self::Input
                | Self::Link
                | Self::Meta
        )
    }

    pub const fn is_raw_text(self) -> bool {
        matches!(self, Self::Script | Self::Style | Self::Textarea)
    }

    pub const fn is_formatting(self) -> bool {
        matches!(
            self,
            Self::A
                | Self::B
                | Self::Big
                | Self::Code
                | Self::Em
                | Self::Font
                | Self::I
                | Self::Nobr
                | Self::S
                | Self::Small
                | Self::Strike
                | Self::Strong
                | Self::Tt
                | Self::U
        )
    }

    pub const fn is_block(self) -> bool {
        matches!(
            self,
            Self::Address
                | Self::Article
                | Self::Aside
                | Self::Blockquote
                | Self::Body
                | Self::Caption
                | Self::Dd
                | Self::Div
                | Self::Dl
                | Self::Dt
                | Self::Details
                | Self::Dialog
                | Self::Fieldset
                | Self::Figcaption
                | Self::Figure
                | Self::Footer
                | Self::Form
                | Self::H(_)
                | Self::Hgroup
                | Self::Header
                | Self::Hr
                | Self::Li
                | Self::Main
                | Self::Menu
                | Self::Nav
                | Self::Ol
                | Self::P
                | Self::Pre
                | Self::Search
                | Self::Section
                | Self::Summary
                | Self::Table
                | Self::Tbody
                | Self::Td
                | Self::Tfoot
                | Self::Th
                | Self::Thead
                | Self::Tr
                | Self::Ul
        )
    }

    pub const fn is_table_structural(self) -> bool {
        matches!(
            self,
            Self::Table
                | Self::Caption
                | Self::Colgroup
                | Self::Col
                | Self::Thead
                | Self::Tbody
                | Self::Tfoot
                | Self::Tr
                | Self::Td
                | Self::Th
        )
    }

    pub const fn is_omitted_subtree(self) -> bool {
        matches!(
            self,
            Self::Script
                | Self::Style
                | Self::Template
                | Self::Input
                | Self::Button
                | Self::Select
                | Self::Textarea
                | Self::Iframe
                | Self::Object
                | Self::Embed
                | Self::Svg
                | Self::Math
        )
    }
}

#[derive(Debug, Default)]
pub(super) struct Attributes {
    pub alt: Option<String>,
    pub aria_label: Option<String>,
    pub meta_name: Option<String>,
    pub meta_content: Option<String>,
    pub role: Option<String>,
    pub hidden: bool,
    pub aria_hidden: bool,
    pub display_none: bool,
}

#[derive(Debug)]
pub(super) enum Token {
    StartTag {
        tag: Tag,
        attributes: Attributes,
        self_closing: bool,
    },
    EndTag(Tag),
    Text(String),
    Ignored,
    Eof,
}

pub(super) struct Tokenizer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    position: usize,
    raw_text: Option<Tag>,
    rcdata: Option<Tag>,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            position: 0,
            raw_text: None,
            rcdata: None,
        }
    }

    #[cfg(test)]
    pub(super) const fn position(&self) -> usize {
        self.position
    }

    pub fn next_token(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Token, HtmlError> {
        // Reserve the token before scanning or constructing it. A source whose
        // token allowance is exhausted must not perform one more bounded scan or
        // allocate one more token buffer before the mandatory gate fires.
        budget.token(limits)?;
        if let Some(raw_tag) = self.raw_text.take() {
            self.scan_raw_text(raw_tag, budget, limits)?;
        }
        if self.position == self.bytes.len() {
            return Ok(Token::Eof);
        }
        if let Some(rcdata_tag) = self.rcdata {
            return self.rcdata_token(rcdata_tag, budget, limits);
        }
        let token = if self.bytes[self.position] != b'<' {
            self.text_token(budget, limits)?
        } else if self.starts_with(b"<!--") {
            self.scan_comment(budget, limits)?;
            Token::Ignored
        } else if self.starts_ascii_case_insensitive(b"<!doctype") {
            self.scan_declaration(budget, limits)?;
            Token::Ignored
        } else if self.starts_with(b"<!") || self.starts_with(b"<?") {
            budget.parse_error(limits)?;
            self.scan_declaration(budget, limits)?;
            Token::Ignored
        } else if self.starts_with(b"</") {
            match self.end_tag(budget, limits)? {
                Some(token) => token,
                None => self.literal_less_than(budget, limits)?,
            }
        } else {
            match self.start_tag(budget, limits)? {
                Some(token) => token,
                None => self.literal_less_than(budget, limits)?,
            }
        };
        Ok(token)
    }

    fn text_token(&mut self, budget: &mut Budget, limits: &HtmlLimits) -> Result<Token, HtmlError> {
        let mut output = String::new();
        while self.position < self.bytes.len()
            && self.bytes[self.position] != b'<'
            && output.len() < limits.text_token_bytes
        {
            if self.bytes[self.position] == b'&'
                && let Some((consumed, first, second)) =
                    self.decode_reference(self.position, false, budget, limits)?
            {
                budget.tokenizer_step(limits, consumed)?;
                push_char(&mut output, first, limits.text_token_bytes)?;
                if let Some(second) = second {
                    push_char(&mut output, second, limits.text_token_bytes)?;
                }
                self.position += consumed;
                continue;
            }
            let source_character = self.input[self.position..]
                .chars()
                .next()
                .expect("position is a character boundary");
            let character = if source_character == '\0' {
                budget.parse_error(limits)?;
                '\u{FFFD}'
            } else {
                source_character
            };
            if output.len() + character.len_utf8() > limits.text_token_bytes {
                break;
            }
            budget.tokenizer_step(limits, source_character.len_utf8())?;
            push_char(&mut output, character, limits.text_token_bytes)?;
            self.position += source_character.len_utf8();
        }
        Ok(Token::Text(output))
    }

    fn rcdata_token(
        &mut self,
        tag: Tag,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Token, HtmlError> {
        if self.is_appropriate_rcdata_end(tag) {
            self.rcdata = None;
            return self
                .end_tag(budget, limits)?
                .ok_or_else(|| HtmlError::limit(HtmlStage::Tokenize));
        }

        let mut output = String::new();
        while self.position < self.bytes.len() && output.len() < limits.text_token_bytes {
            if self.is_appropriate_rcdata_end(tag) {
                break;
            }
            if self.bytes[self.position] == b'&'
                && let Some((consumed, first, second)) =
                    self.decode_reference(self.position, false, budget, limits)?
            {
                budget.tokenizer_step(limits, consumed)?;
                push_char(&mut output, first, limits.text_token_bytes)?;
                if let Some(second) = second {
                    push_char(&mut output, second, limits.text_token_bytes)?;
                }
                self.position += consumed;
                continue;
            }
            let source_character = self.input[self.position..]
                .chars()
                .next()
                .expect("position is a character boundary");
            let character = if source_character == '\0' {
                budget.parse_error(limits)?;
                '\u{FFFD}'
            } else {
                source_character
            };
            if output.len() + character.len_utf8() > limits.text_token_bytes {
                break;
            }
            budget.tokenizer_step(limits, source_character.len_utf8())?;
            push_char(&mut output, character, limits.text_token_bytes)?;
            self.position += source_character.len_utf8();
        }
        Ok(Token::Text(output))
    }

    fn is_appropriate_rcdata_end(&self, tag: Tag) -> bool {
        let needle: &[u8] = match tag {
            Tag::Title => b"</title",
            _ => return false,
        };
        self.bytes[self.position..]
            .get(..needle.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
            && self
                .bytes
                .get(self.position + needle.len())
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
    }

    fn literal_less_than(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Token, HtmlError> {
        budget.parse_error(limits)?;
        budget.tokenizer_step(limits, 1)?;
        self.position += 1;
        Ok(Token::Text("<".to_owned()))
    }

    fn start_tag(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Option<Token>, HtmlError> {
        budget.tokenizer_step(limits, 1)?;
        let mut cursor = self.position + 1;
        if cursor >= self.bytes.len() || !is_name_byte(self.bytes[cursor]) {
            return Ok(None);
        }
        let name_start = cursor;
        while cursor < self.bytes.len() && is_name_byte(self.bytes[cursor]) {
            budget.tokenizer_step(limits, 1)?;
            cursor += 1;
            if cursor - name_start > limits.name_bytes {
                return Err(HtmlError::limit(HtmlStage::Tokenize));
            }
        }
        let mut lowered = [0_u8; 1_024];
        let name_len = cursor - name_start;
        for (target, source) in lowered[..name_len]
            .iter_mut()
            .zip(&self.bytes[name_start..cursor])
        {
            *target = source.to_ascii_lowercase();
        }
        let tag = Tag::from_name(&lowered[..name_len]);
        let mut attributes = Attributes::default();
        let mut seen = 0_u16;
        let mut attribute_count = 0_usize;
        let mut attribute_bytes = 0_usize;
        let mut self_closing = false;

        loop {
            skip_ascii_whitespace(self.bytes, &mut cursor, budget, limits)?;
            if cursor >= self.bytes.len() {
                budget.parse_error(limits)?;
                self.position = cursor;
                break;
            }
            if self.bytes[cursor] == b'>' {
                budget.tokenizer_step(limits, 1)?;
                cursor += 1;
                self.position = cursor;
                break;
            }
            if self.bytes[cursor] == b'/' && self.bytes.get(cursor + 1) == Some(&b'>') {
                budget.tokenizer_step(limits, 2)?;
                self_closing = true;
                cursor += 2;
                self.position = cursor;
                break;
            }

            attribute_count = attribute_count
                .checked_add(1)
                .filter(|count| *count <= limits.attributes_per_element)
                .ok_or_else(|| HtmlError::limit(HtmlStage::Tokenize))?;
            let attr_start = cursor;
            while cursor < self.bytes.len()
                && !is_attribute_separator(self.bytes[cursor])
                && self.bytes[cursor] != b'='
            {
                budget.tokenizer_step(limits, 1)?;
                cursor += 1;
                if cursor - attr_start > limits.name_bytes {
                    return Err(HtmlError::limit(HtmlStage::Tokenize));
                }
            }
            if cursor == attr_start {
                budget.parse_error(limits)?;
                budget.tokenizer_step(limits, 1)?;
                cursor += 1;
                continue;
            }
            let name = &self.bytes[attr_start..cursor];
            skip_ascii_whitespace(self.bytes, &mut cursor, budget, limits)?;
            let value = if self.bytes.get(cursor) == Some(&b'=') {
                budget.tokenizer_step(limits, 1)?;
                cursor += 1;
                skip_ascii_whitespace(self.bytes, &mut cursor, budget, limits)?;
                scan_attribute_value(self.bytes, &mut cursor, budget, limits)?
            } else {
                cursor..cursor
            };
            let cost = name
                .len()
                .checked_add(value.len())
                .ok_or_else(|| HtmlError::limit(HtmlStage::Tokenize))?;
            attribute_bytes = attribute_bytes
                .checked_add(cost)
                .filter(|bytes| *bytes <= limits.attribute_bytes_per_element)
                .ok_or_else(|| HtmlError::limit(HtmlStage::Tokenize))?;
            budget.attributes(limits, 1, cost)?;

            if let Some(kind) = AttributeKind::from_ascii_case_insensitive(name) {
                let bit = 1_u16 << kind as u16;
                if seen & bit == 0 {
                    seen |= bit;
                    self.retain_attribute(kind, value, &mut attributes, budget, limits)?;
                }
            }
        }

        if tag == Tag::Title && !self_closing {
            self.rcdata = Some(tag);
        } else if tag.is_raw_text() && !tag.is_void() && !self_closing {
            self.raw_text = Some(tag);
        }
        Ok(Some(Token::StartTag {
            tag,
            attributes,
            self_closing,
        }))
    }

    fn retain_attribute(
        &self,
        kind: AttributeKind,
        value: std::ops::Range<usize>,
        attributes: &mut Attributes,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        match kind {
            AttributeKind::Hidden => attributes.hidden = true,
            AttributeKind::AriaHidden => {
                attributes.aria_hidden = self.bytes[value.clone()].eq_ignore_ascii_case(b"true")
            }
            AttributeKind::Style => {
                attributes.display_none = inline_display_none(&self.bytes[value])?;
            }
            AttributeKind::Alt
            | AttributeKind::AriaLabel
            | AttributeKind::Name
            | AttributeKind::Content
            | AttributeKind::Role => {
                let decoded = decode_attribute_fragment(self.input, value, budget, limits)?;
                match kind {
                    AttributeKind::Alt => attributes.alt = Some(decoded),
                    AttributeKind::AriaLabel => attributes.aria_label = Some(decoded),
                    AttributeKind::Name => attributes.meta_name = Some(decoded),
                    AttributeKind::Content => attributes.meta_content = Some(decoded),
                    AttributeKind::Role => attributes.role = Some(decoded),
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }

    fn end_tag(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Option<Token>, HtmlError> {
        budget.tokenizer_step(limits, 2)?;
        let mut cursor = self.position + 2;
        skip_ascii_whitespace(self.bytes, &mut cursor, budget, limits)?;
        let name_start = cursor;
        while cursor < self.bytes.len() && is_name_byte(self.bytes[cursor]) {
            budget.tokenizer_step(limits, 1)?;
            cursor += 1;
            if cursor - name_start > limits.name_bytes {
                return Err(HtmlError::limit(HtmlStage::Tokenize));
            }
        }
        if cursor == name_start {
            return Ok(None);
        }
        let mut lowered = [0_u8; 1_024];
        let name_len = cursor - name_start;
        for (target, source) in lowered[..name_len]
            .iter_mut()
            .zip(&self.bytes[name_start..cursor])
        {
            *target = source.to_ascii_lowercase();
        }
        while cursor < self.bytes.len() && self.bytes[cursor] != b'>' {
            budget.tokenizer_step(limits, 1)?;
            cursor += 1;
        }
        if cursor < self.bytes.len() {
            budget.tokenizer_step(limits, 1)?;
            cursor += 1;
        } else {
            budget.parse_error(limits)?;
        }
        self.position = cursor;
        Ok(Some(Token::EndTag(Tag::from_name(&lowered[..name_len]))))
    }

    fn scan_comment(&mut self, budget: &mut Budget, limits: &HtmlLimits) -> Result<(), HtmlError> {
        budget.tokenizer_step(limits, 4)?;
        self.position += 4;
        let mut scanned = 0_usize;
        while self.position < self.bytes.len() {
            let window_start = self.position.saturating_sub(2);
            let end = (self.position + OMITTED_SCAN_BUFFER_BYTES).min(self.bytes.len());
            if let Some(offset) = find_subslice(&self.bytes[window_start..end], b"-->") {
                let target = window_start + offset + 3;
                budget.tokenizer_step(limits, target.saturating_sub(self.position))?;
                self.position = target;
                return Ok(());
            }
            let consumed = end - self.position;
            budget.tokenizer_step(limits, consumed)?;
            scanned += consumed;
            self.position = end;
            if scanned > self.bytes.len() {
                return Err(HtmlError::limit(HtmlStage::Tokenize));
            }
        }
        budget.parse_error(limits)?;
        Ok(())
    }

    fn scan_declaration(
        &mut self,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        budget.tokenizer_step(limits, 2)?;
        self.position += 2;
        while self.position < self.bytes.len() {
            let end = (self.position + OMITTED_SCAN_BUFFER_BYTES).min(self.bytes.len());
            if let Some(offset) = self.bytes[self.position..end]
                .iter()
                .position(|byte| *byte == b'>')
            {
                budget.tokenizer_step(limits, offset + 1)?;
                self.position += offset + 1;
                return Ok(());
            }
            let consumed = end - self.position;
            budget.tokenizer_step(limits, consumed)?;
            self.position = end;
        }
        Ok(())
    }

    fn scan_raw_text(
        &mut self,
        raw_tag: Tag,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<(), HtmlError> {
        let needle: &[u8] = match raw_tag {
            Tag::Script => b"</script",
            Tag::Style => b"</style",
            Tag::Textarea => b"</textarea",
            _ => return Ok(()),
        };
        while self.position < self.bytes.len() {
            let end = (self.position + OMITTED_SCAN_BUFFER_BYTES).min(self.bytes.len());
            let window_start = self.position.saturating_sub(needle.len());
            if let Some(offset) =
                find_ascii_case_insensitive(&self.bytes[window_start..end], needle)
            {
                let target = window_start + offset;
                budget.tokenizer_step(limits, target.saturating_sub(self.position))?;
                self.position = target;
                return Ok(());
            }
            let consumed = end - self.position;
            budget.tokenizer_step(limits, consumed)?;
            self.position = end;
        }
        budget.parse_error(limits)?;
        Ok(())
    }

    fn decode_reference(
        &self,
        ampersand: usize,
        in_attribute: bool,
        budget: &mut Budget,
        limits: &HtmlLimits,
    ) -> Result<Option<(usize, char, Option<char>)>, HtmlError> {
        decode_reference_at(self.input, ampersand, in_attribute, budget, limits)
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.bytes[self.position..].starts_with(needle)
    }

    fn starts_ascii_case_insensitive(&self, needle: &[u8]) -> bool {
        self.bytes[self.position..]
            .get(..needle.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum AttributeKind {
    Alt,
    AriaLabel,
    Name,
    Content,
    Hidden,
    AriaHidden,
    Role,
    Style,
}

impl AttributeKind {
    fn from_ascii_case_insensitive(name: &[u8]) -> Option<Self> {
        if name.eq_ignore_ascii_case(b"alt") {
            Some(Self::Alt)
        } else if name.eq_ignore_ascii_case(b"aria-label") {
            Some(Self::AriaLabel)
        } else if name.eq_ignore_ascii_case(b"name") {
            Some(Self::Name)
        } else if name.eq_ignore_ascii_case(b"content") {
            Some(Self::Content)
        } else if name.eq_ignore_ascii_case(b"hidden") {
            Some(Self::Hidden)
        } else if name.eq_ignore_ascii_case(b"aria-hidden") {
            Some(Self::AriaHidden)
        } else if name.eq_ignore_ascii_case(b"role") {
            Some(Self::Role)
        } else if name.eq_ignore_ascii_case(b"style") {
            Some(Self::Style)
        } else {
            None
        }
    }
}

fn decode_attribute_fragment(
    input: &str,
    range: std::ops::Range<usize>,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<String, HtmlError> {
    let mut output = String::new();
    let mut position = range.start;
    while position < range.end {
        if input.as_bytes()[position] == b'&'
            && let Some((consumed, first, second)) =
                decode_reference_at(input, position, true, budget, limits)?
            && position + consumed <= range.end
        {
            push_char(&mut output, first, limits.attribute_bytes_per_element)?;
            if let Some(second) = second {
                push_char(&mut output, second, limits.attribute_bytes_per_element)?;
            }
            position += consumed;
            continue;
        }
        let character = input[position..]
            .chars()
            .next()
            .expect("attribute position is a character boundary");
        push_char(&mut output, character, limits.attribute_bytes_per_element)?;
        position += character.len_utf8();
    }
    Ok(output)
}

fn decode_reference_at(
    input: &str,
    ampersand: usize,
    in_attribute: bool,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<Option<(usize, char, Option<char>)>, HtmlError> {
    budget.character_reference(limits)?;
    let bytes = input.as_bytes();
    let mut cursor = ampersand + 1;
    if bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
        let hexadecimal = bytes
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b'x' | b'X'));
        if hexadecimal {
            cursor += 1;
        }
        let digit_start = cursor;
        let mut value = 0_u32;
        while let Some(byte) = bytes.get(cursor).copied() {
            let digit = if hexadecimal {
                byte.to_digit(16)
            } else {
                byte.to_digit(10)
            };
            let Some(digit) = digit else { break };
            budget.character_reference_steps(limits, 1)?;
            value = value.saturating_mul(if hexadecimal { 16 } else { 10 });
            value = value.saturating_add(digit);
            cursor += 1;
            if cursor - digit_start > limits.character_reference_scratch {
                return Err(HtmlError::limit(HtmlStage::Tokenize));
            }
        }
        if cursor == digit_start {
            return Ok(None);
        }
        if bytes.get(cursor) == Some(&b';') {
            cursor += 1;
        }
        let character = numeric_reference(value);
        return Ok(Some((cursor - ampersand, character, None)));
    }

    let candidate_start = cursor;
    while let Some(byte) = bytes.get(cursor).copied() {
        if !byte.is_ascii_alphanumeric() {
            if byte == b';' && cursor - candidate_start < limits.character_reference_scratch {
                cursor += 1;
            }
            break;
        }
        if cursor - candidate_start == limits.character_reference_scratch {
            break;
        }
        cursor += 1;
    }
    if cursor == candidate_start {
        return Ok(None);
    }
    let candidate = &bytes[candidate_start..cursor];
    budget.character_reference_steps(limits, candidate.len())?;
    let Some((matched, first, second)) = longest_named_reference(candidate) else {
        return Ok(None);
    };
    let semicolon = candidate.get(matched.wrapping_sub(1)) == Some(&b';');
    if in_attribute
        && !semicolon
        && let Some(next) = bytes.get(candidate_start + matched).copied()
        && (next.is_ascii_alphanumeric() || next == b'=')
    {
        return Ok(None);
    }
    Ok(Some((1 + matched, first, second)))
}

fn numeric_reference(value: u32) -> char {
    let mapped = match value {
        0x80 => 0x20AC,
        0x82 => 0x201A,
        0x83 => 0x0192,
        0x84 => 0x201E,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02C6,
        0x89 => 0x2030,
        0x8A => 0x0160,
        0x8B => 0x2039,
        0x8C => 0x0152,
        0x8E => 0x017D,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201C,
        0x94 => 0x201D,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02DC,
        0x99 => 0x2122,
        0x9A => 0x0161,
        0x9B => 0x203A,
        0x9C => 0x0153,
        0x9E => 0x017E,
        0x9F => 0x0178,
        other => other,
    };
    if mapped == 0 || mapped > 0x10_FFFF || (0xD800..=0xDFFF).contains(&mapped) {
        '\u{FFFD}'
    } else {
        char::from_u32(mapped).unwrap_or('\u{FFFD}')
    }
}

fn inline_display_none(value: &[u8]) -> Result<bool, HtmlError> {
    // CSS comments are removed before declaration tokenization. Normalize into a
    // fallibly allocated bounded buffer so comments around `display`, its colon,
    // or its value cannot turn a valid declaration into promoted primary text.
    let mut normalized = Vec::new();
    normalized
        .try_reserve_exact(value.len())
        .map_err(|_| HtmlError::limit(HtmlStage::Tokenize))?;
    let mut cursor = 0_usize;
    while cursor < value.len() {
        if value[cursor..].starts_with(b"/*") {
            cursor += 2;
            while cursor < value.len() && !value[cursor..].starts_with(b"*/") {
                cursor += 1;
            }
            if cursor < value.len() {
                cursor += 2;
            }
        } else {
            normalized.push(value[cursor]);
            cursor += 1;
        }
    }

    let mut display = None;
    for declaration in normalized.split(|byte| *byte == b';') {
        let Some(colon) = declaration.iter().position(|byte| *byte == b':') else {
            continue;
        };
        let name = trim_ascii(&declaration[..colon]);
        if !name.eq_ignore_ascii_case(b"display") {
            continue;
        }
        let mut value = trim_ascii(&declaration[colon + 1..]);
        if let Some(important) = find_ascii_case_insensitive(value, b"!important")
            && trim_ascii(&value[important + b"!important".len()..]).is_empty()
        {
            value = trim_ascii(&value[..important]);
        }
        if let Some(next) = valid_display_state(value) {
            display = Some(next);
        }
    }
    Ok(display.unwrap_or(false))
}

fn valid_display_state(value: &[u8]) -> Option<bool> {
    let mut words = value
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|word| !word.is_empty());
    let first = words.next()?;
    let second = words.next();
    let third = words.next();
    if words.next().is_some() {
        return None;
    }

    match (second, third) {
        (None, None) if first.eq_ignore_ascii_case(b"none") => Some(true),
        (None, None) if is_single_display_keyword(first) => Some(false),
        (Some(second), None)
            if (is_display_outside(first) && is_display_inside(second))
                || is_two_keyword_list_item(first, second) =>
        {
            Some(false)
        }
        (Some(second), Some(third)) if is_three_keyword_list_item(first, second, third) => {
            Some(false)
        }
        _ => None,
    }
}

fn is_single_display_keyword(word: &[u8]) -> bool {
    [
        b"block".as_slice(),
        b"contents",
        b"flex",
        b"flow",
        b"flow-root",
        b"grid",
        b"inherit",
        b"initial",
        b"inline",
        b"inline-block",
        b"inline-flex",
        b"inline-grid",
        b"inline-table",
        b"list-item",
        b"revert",
        b"revert-layer",
        b"ruby",
        b"ruby-base",
        b"ruby-base-container",
        b"ruby-text",
        b"ruby-text-container",
        b"run-in",
        b"table",
        b"table-caption",
        b"table-cell",
        b"table-column",
        b"table-column-group",
        b"table-footer-group",
        b"table-header-group",
        b"table-row",
        b"table-row-group",
        b"unset",
    ]
    .iter()
    .any(|candidate| word.eq_ignore_ascii_case(candidate))
}

fn is_display_outside(word: &[u8]) -> bool {
    word.eq_ignore_ascii_case(b"block")
        || word.eq_ignore_ascii_case(b"inline")
        || word.eq_ignore_ascii_case(b"run-in")
}

fn is_display_inside(word: &[u8]) -> bool {
    word.eq_ignore_ascii_case(b"flow")
        || word.eq_ignore_ascii_case(b"flow-root")
        || word.eq_ignore_ascii_case(b"table")
        || word.eq_ignore_ascii_case(b"flex")
        || word.eq_ignore_ascii_case(b"grid")
        || word.eq_ignore_ascii_case(b"ruby")
}

fn is_two_keyword_list_item(first: &[u8], second: &[u8]) -> bool {
    (first.eq_ignore_ascii_case(b"list-item")
        && (is_display_outside(second)
            || second.eq_ignore_ascii_case(b"flow")
            || second.eq_ignore_ascii_case(b"flow-root")))
        || (second.eq_ignore_ascii_case(b"list-item")
            && (is_display_outside(first)
                || first.eq_ignore_ascii_case(b"flow")
                || first.eq_ignore_ascii_case(b"flow-root")))
}

fn is_three_keyword_list_item(first: &[u8], second: &[u8], third: &[u8]) -> bool {
    let words = [first, second, third];
    words
        .iter()
        .filter(|word| word.eq_ignore_ascii_case(b"list-item"))
        .count()
        == 1
        && words.iter().filter(|word| is_display_outside(word)).count() == 1
        && words
            .iter()
            .filter(|word| {
                word.eq_ignore_ascii_case(b"flow") || word.eq_ignore_ascii_case(b"flow-root")
            })
            .count()
            == 1
}

fn scan_attribute_value(
    bytes: &[u8],
    cursor: &mut usize,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<std::ops::Range<usize>, HtmlError> {
    let quote = bytes
        .get(*cursor)
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'));
    if let Some(quote) = quote {
        budget.tokenizer_step(limits, 1)?;
        *cursor += 1;
        let start = *cursor;
        while *cursor < bytes.len() && bytes[*cursor] != quote {
            budget.tokenizer_step(limits, 1)?;
            *cursor += 1;
        }
        let end = *cursor;
        if *cursor < bytes.len() {
            budget.tokenizer_step(limits, 1)?;
            *cursor += 1;
        }
        Ok(start..end)
    } else {
        let start = *cursor;
        while *cursor < bytes.len()
            && !bytes[*cursor].is_ascii_whitespace()
            && !matches!(bytes[*cursor], b'>' | b'<')
        {
            budget.tokenizer_step(limits, 1)?;
            *cursor += 1;
        }
        Ok(start..*cursor)
    }
}

fn push_char(output: &mut String, character: char, ceiling: usize) -> Result<(), HtmlError> {
    let new_len = output
        .len()
        .checked_add(character.len_utf8())
        .filter(|length| *length <= ceiling)
        .ok_or_else(|| HtmlError::limit(HtmlStage::Tokenize))?;
    if new_len > output.capacity() {
        output
            .try_reserve_exact((new_len - output.capacity()).clamp(64, 8_192))
            .map_err(|_| HtmlError::limit(HtmlStage::Tokenize))?;
    }
    output.push(character);
    Ok(())
}

fn stable_name_hash(name: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in name {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':')
}

fn is_attribute_separator(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>')
}

fn skip_ascii_whitespace(
    bytes: &[u8],
    cursor: &mut usize,
    budget: &mut Budget,
    limits: &HtmlLimits,
) -> Result<(), HtmlError> {
    while bytes
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        budget.tokenizer_step(limits, 1)?;
        *cursor += 1;
    }
    Ok(())
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|candidate| candidate.eq_ignore_ascii_case(needle))
}

trait AsciiDigit {
    fn to_digit(self, radix: u32) -> Option<u32>;
}

impl AsciiDigit for u8 {
    fn to_digit(self, radix: u32) -> Option<u32> {
        (self as char).to_digit(radix)
    }
}
