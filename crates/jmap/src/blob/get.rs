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
    error::method::MethodError,
    method::{
        get::{GetRequest, GetResponse},
        lookup::{BlobInfo, BlobLookupRequest, BlobLookupResponse},
    },
    object::{blob::GetArguments, Object},
    types::{
        collection::Collection,
        id::Id,
        property::{DataProperty, DigestProperty, Property},
        type_state::DataType,
        value::Value,
        MaybeUnparsable,
    },
};
use mail_builder::encoders::base64::base64_encode;
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha512};
use store::BlobKind;
use utils::map::vec_map::VecMap;

use crate::{auth::AccessToken, JMAP};

impl JMAP {
    pub async fn blob_get(
        &self,
        mut request: GetRequest<GetArguments>,
        access_token: &AccessToken,
    ) -> Result<GetResponse, MethodError> {
        let ids = request
            .unwrap_blob_ids(self.config.get_max_objects)?
            .unwrap_or_default();
        let properties = request.unwrap_properties(&[
            Property::Id,
            Property::Data(DataProperty::Default),
            Property::Size,
        ]);
        let mut response = GetResponse {
            account_id: request.account_id.into(),
            state: None,
            list: Vec::with_capacity(ids.len()),
            not_found: vec![],
        };

        let range_from = request.arguments.offset.unwrap_or(0);
        let range_to = request
            .arguments
            .length
            .map(|length| range_from.saturating_add(length))
            .unwrap_or(usize::MAX);

        for blob_id in ids {
            if let Some(bytes) = self.blob_download(&blob_id, access_token).await? {
                let mut blob = Object::with_capacity(properties.len());
                let bytes_range = if range_from == 0 && range_to == usize::MAX {
                    &bytes[..]
                } else {
                    let range_to = if range_to != usize::MAX && range_to > bytes.len() {
                        blob.append(Property::IsTruncated, true);
                        bytes.len()
                    } else {
                        range_to
                    };
                    let bytes_range = bytes.get(range_from..range_to).unwrap_or_default();
                    bytes_range
                };

                for property in &properties {
                    let mut property = property.clone();
                    let value: Value = match &property {
                        Property::Id => Value::BlobId(blob_id.clone()),
                        Property::Size => bytes.len().into(),
                        Property::Digest(digest) => match digest {
                            DigestProperty::Sha => {
                                let mut hasher = Sha1::new();
                                hasher.update(bytes_range);
                                String::from_utf8(
                                    base64_encode(&hasher.finalize()[..]).unwrap_or_default(),
                                )
                                .unwrap()
                            }
                            DigestProperty::Sha256 => {
                                let mut hasher = Sha256::new();
                                hasher.update(bytes_range);
                                String::from_utf8(
                                    base64_encode(&hasher.finalize()[..]).unwrap_or_default(),
                                )
                                .unwrap()
                            }
                            DigestProperty::Sha512 => {
                                let mut hasher = Sha512::new();
                                hasher.update(bytes_range);
                                String::from_utf8(
                                    base64_encode(&hasher.finalize()[..]).unwrap_or_default(),
                                )
                                .unwrap()
                            }
                        }
                        .into(),
                        Property::Data(data) => match data {
                            DataProperty::AsText => match std::str::from_utf8(bytes_range) {
                                Ok(text) => text.to_string().into(),
                                Err(_) => {
                                    blob.append(Property::IsEncodingProblem, true);
                                    Value::Null
                                }
                            },
                            DataProperty::AsBase64 => {
                                String::from_utf8(base64_encode(bytes_range).unwrap_or_default())
                                    .unwrap()
                                    .into()
                            }
                            DataProperty::Default => match std::str::from_utf8(bytes_range) {
                                Ok(text) => {
                                    property = Property::Data(DataProperty::AsText);
                                    text.to_string().into()
                                }
                                Err(_) => {
                                    property = Property::Data(DataProperty::AsBase64);
                                    blob.append(Property::IsEncodingProblem, true);
                                    String::from_utf8(
                                        base64_encode(bytes_range).unwrap_or_default(),
                                    )
                                    .unwrap()
                                    .into()
                                }
                            },
                        },
                        _ => Value::Null,
                    };
                    blob.append(property, value);
                }

                // Add result to response
                response.list.push(blob);
            } else {
                response.not_found.push(blob_id.into());
            }
        }

        Ok(response)
    }

    pub async fn blob_lookup(
        &self,
        request: BlobLookupRequest,
    ) -> Result<BlobLookupResponse, MethodError> {
        let mut include_email = false;
        let mut include_mailbox = false;
        let mut include_thread = false;

        let type_names = request
            .type_names
            .into_iter()
            .map(|tn| match tn {
                MaybeUnparsable::Value(value) => {
                    match &value {
                        DataType::Email => {
                            include_email = true;
                        }
                        DataType::Mailbox => {
                            include_mailbox = true;
                        }
                        DataType::Thread => {
                            include_thread = true;
                        }
                        _ => (),
                    }

                    Ok(value)
                }
                MaybeUnparsable::ParseError(_) => Err(MethodError::UnknownDataType),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let req_account_id = request.account_id.document_id();
        let mut response = BlobLookupResponse {
            account_id: request.account_id,
            list: Vec::with_capacity(request.ids.len()),
            not_found: vec![],
        };

        for id in request.ids {
            match id {
                MaybeUnparsable::Value(id) => {
                    let mut matched_ids = VecMap::new();

                    match &id.kind {
                        BlobKind::Linked {
                            account_id,
                            collection,
                            document_id,
                        } if *account_id == req_account_id => {
                            if *account_id != req_account_id {
                                response.not_found.push(MaybeUnparsable::Value(id));
                                continue;
                            }

                            match DataType::try_from(Collection::from(*collection)) {
                                Ok(data_type) if type_names.contains(&data_type) => {
                                    matched_ids.append(data_type, vec![Id::from(*document_id)]);
                                }
                                _ => (),
                            }
                        }
                        BlobKind::LinkedMaildir {
                            account_id,
                            document_id,
                        } if *account_id == req_account_id => {
                            if include_email || include_thread {
                                if let Some(thread_id) = self
                                    .get_property::<u32>(
                                        req_account_id,
                                        Collection::Email,
                                        *document_id,
                                        Property::ThreadId,
                                    )
                                    .await?
                                {
                                    if include_email {
                                        matched_ids.append(
                                            DataType::Email,
                                            vec![Id::from_parts(thread_id, *document_id)],
                                        );
                                    }
                                    if include_thread {
                                        matched_ids
                                            .append(DataType::Thread, vec![Id::from(thread_id)]);
                                    }
                                }
                            }
                            if include_mailbox {
                                if let Some(mailboxes) = self
                                    .get_property::<Vec<u32>>(
                                        req_account_id,
                                        Collection::Email,
                                        *document_id,
                                        Property::MailboxIds,
                                    )
                                    .await?
                                {
                                    matched_ids.append(
                                        DataType::Mailbox,
                                        mailboxes.into_iter().map(Id::from).collect::<Vec<_>>(),
                                    );
                                }
                            }
                        }
                        BlobKind::Temporary { account_id, .. } if *account_id == req_account_id => {
                        }
                        _ => {
                            response.not_found.push(MaybeUnparsable::Value(id));
                            continue;
                        }
                    }

                    response.list.push(BlobInfo { id, matched_ids });
                }
                _ => response.not_found.push(id),
            }
        }

        Ok(response)
    }
}
