/*
 * Copyright (c) 2023 Stalwart Labs Ltd.
 *
 * This file is part of Stalwart Mail Server.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use jmap_proto::{
    object::Object,
    types::{blob::BlobId, property::Property, value::Value},
};
use mail_parser::{HeaderValue, MessagePart, MimeHeaders, PartType};

use super::headers::HeaderToValue;

pub trait ToBodyPart {
    fn to_body_part(
        &self,
        part_id: usize,
        properties: &[Property],
        raw_message: &[u8],
        blob_id: &BlobId,
    ) -> Value;
}

impl ToBodyPart for Vec<MessagePart<'_>> {
    fn to_body_part(
        &self,
        part_id: usize,
        properties: &[Property],
        raw_message: &[u8],
        blob_id: &BlobId,
    ) -> Value {
        let mut parts = vec![part_id].into_iter();
        let mut parts_stack = Vec::new();
        let mut subparts = Vec::with_capacity(1);

        loop {
            if let Some((part_id, part)) = parts.next().map(|part_id| (part_id, &self[part_id])) {
                let mut values = Object::with_capacity(properties.len());
                let multipart = if let PartType::Multipart(parts) = &part.body {
                    parts.into()
                } else {
                    None
                };

                for property in properties {
                    let value = match property {
                        Property::PartId if multipart.is_none() => part_id.to_string().into(),
                        Property::BlobId if multipart.is_none() => {
                            let base_offset = blob_id.start_offset();
                            BlobId::new_section(
                                blob_id.kind,
                                part.offset_body + base_offset,
                                part.offset_end + base_offset,
                                part.encoding as u8,
                            )
                            .into()
                        }
                        Property::Size if multipart.is_none() => match &part.body {
                            PartType::Text(text) | PartType::Html(text) => text.len(),
                            PartType::Binary(bin) | PartType::InlineBinary(bin) => bin.len(),
                            PartType::Message(message) => message.root_part().raw_len(),
                            PartType::Multipart(_) => 0,
                        }
                        .into(),
                        Property::Name => part.attachment_name().into(),
                        Property::Type => part
                            .content_type()
                            .map(|ct| {
                                ct.subtype()
                                    .map(|st| format!("{}/{}", ct.ctype(), st))
                                    .unwrap_or_else(|| ct.ctype().to_string())
                            })
                            .or_else(|| match &part.body {
                                PartType::Text(_) => Some("text/plain".to_string()),
                                PartType::Html(_) => Some("text/html".to_string()),
                                PartType::Message(_) => Some("message/rfc822".to_string()),
                                _ => None,
                            })
                            .into(),
                        Property::Charset => part
                            .content_type()
                            .and_then(|ct| ct.attribute("charset"))
                            .or(match &part.body {
                                PartType::Text(_) | PartType::Html(_) => Some("us-ascii"),
                                _ => None,
                            })
                            .into(),
                        Property::Disposition => {
                            part.content_disposition().map(|cd| cd.ctype()).into()
                        }
                        Property::Cid => part.content_id().into(),
                        Property::Language => match part.content_language() {
                            HeaderValue::Text(text) => vec![text.to_string()].into(),
                            HeaderValue::TextList(list) => list
                                .iter()
                                .map(|text| text.to_string().into())
                                .collect::<Vec<Value>>()
                                .into(),
                            _ => Value::Null,
                        },
                        Property::Location => part.content_location().into(),
                        Property::Header(_) => part.header_to_value(property, raw_message),
                        Property::Headers => part.headers_to_value(raw_message),
                        Property::SubParts => continue,
                        _ => Value::Null,
                    };
                    values.append(property.clone(), value);
                }

                subparts.push(values);

                if let Some(multipart) = multipart {
                    let multipart = multipart.clone();
                    parts_stack.push((
                        parts,
                        std::mem::replace(&mut subparts, Vec::with_capacity(multipart.len())),
                    ));
                    parts = multipart.into_iter();
                }
            } else if let Some((prev_parts, mut prev_subparts)) = parts_stack.pop() {
                prev_subparts
                    .last_mut()
                    .unwrap()
                    .append(Property::SubParts, subparts);
                parts = prev_parts;
                subparts = prev_subparts;
            } else {
                return subparts.pop().map(Into::into).unwrap_or_default();
            }
        }
    }
}

pub(super) trait TruncateBody {
    fn truncate(&self, max_len: usize) -> (bool, String);
}

impl TruncateBody for PartType<'_> {
    fn truncate(&self, mut max_len: usize) -> (bool, String) {
        match self {
            PartType::Text(text) => {
                if max_len != 0 && text.len() > max_len {
                    let add_dots = max_len > 6;
                    if add_dots {
                        max_len -= 3;
                    }
                    let mut result = String::with_capacity(max_len);
                    for ch in text.chars() {
                        if ch != '\r' {
                            if ch.len_utf8() + result.len() > max_len {
                                break;
                            }
                            result.push(ch);
                        }
                    }
                    if add_dots {
                        result.push_str("...");
                    }
                    (true, result)
                } else {
                    (false, text.replace('\r', ""))
                }
            }
            PartType::Html(html) => {
                if max_len != 0 && html.len() > max_len {
                    let add_dots = max_len > 6;
                    if add_dots {
                        max_len -= 3;
                    }

                    let mut result = String::with_capacity(max_len);
                    let mut in_tag = false;
                    let mut in_comment = false;
                    let mut last_tag_end_pos = 0;
                    let mut cr_count = 0;
                    for (pos, ch) in html.char_indices() {
                        let mut set_last_tag = 0;
                        match ch {
                            '<' if !in_tag => {
                                in_tag = true;
                                if let Some("!--") = html.get(pos + 1..pos + 4) {
                                    in_comment = true;
                                }
                                set_last_tag = pos;
                            }
                            '>' if in_tag => {
                                if in_comment {
                                    if let Some("--") = html.get(pos - 2..pos) {
                                        in_comment = false;
                                        in_tag = false;
                                        set_last_tag = pos + 1;
                                    }
                                } else {
                                    in_tag = false;
                                    set_last_tag = pos + 1;
                                }
                            }
                            '\r' => {
                                cr_count += 1;
                                continue;
                            }
                            _ => (),
                        }
                        if ch.len_utf8() + pos - cr_count > max_len {
                            result.push_str(
                                &html[0..if (in_tag || set_last_tag > 0) && last_tag_end_pos > 0 {
                                    last_tag_end_pos
                                } else {
                                    pos
                                }]
                                    .replace('\r', ""),
                            );
                            if add_dots {
                                result.push_str("...");
                            }
                            break;
                        } else if set_last_tag > 0 {
                            last_tag_end_pos = set_last_tag;
                        }
                    }
                    (true, result)
                } else {
                    (false, html.replace('\r', ""))
                }
            }
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => {
                PartType::Text(String::from_utf8_lossy(bytes)).truncate(max_len)
            }
            _ => (false, "".into()),
        }
    }
}
