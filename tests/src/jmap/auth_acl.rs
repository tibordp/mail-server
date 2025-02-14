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

use std::sync::Arc;

use jmap::{
    mailbox::{INBOX_ID, TRASH_ID},
    JMAP,
};
use jmap_client::{
    client::Client,
    core::{
        error::{MethodError, MethodErrorType},
        set::{SetError, SetErrorType},
    },
    email::{self, import::EmailImportResponse, query::Filter, Property},
    mailbox::{self, Role},
    principal::ACL,
};
use jmap_proto::types::id::Id;
use std::fmt::Debug;
use store::ahash::AHashMap;

use crate::{
    directory::sql::{
        add_to_group, create_test_group_with_email, create_test_user_with_email, remove_from_group,
    },
    jmap::{mailbox::destroy_all_mailboxes, test_account_login},
};

pub async fn test(server: Arc<JMAP>, admin_client: &mut Client) {
    println!("Running ACL tests...");

    // Create a group and three test accounts
    let inbox_id = Id::new(INBOX_ID as u64).to_string();
    let trash_id = Id::new(TRASH_ID as u64).to_string();

    let directory = server.directory.as_ref();
    create_test_user_with_email(directory, "jdoe@example.com", "12345", "John Doe").await;
    create_test_user_with_email(directory, "jane.smith@example.com", "abcde", "Jane Smith").await;
    create_test_user_with_email(directory, "bill@example.com", "098765", "Bill Foobar").await;
    create_test_group_with_email(directory, "sales@example.com", "Sales Group").await;
    let john_id: Id = server
        .get_account_id("jdoe@example.com")
        .await
        .unwrap()
        .into();
    let jane_id: Id = server
        .get_account_id("jane.smith@example.com")
        .await
        .unwrap()
        .into();
    let bill_id: Id = server
        .get_account_id("bill@example.com")
        .await
        .unwrap()
        .into();
    let sales_id: Id = server
        .get_account_id("sales@example.com")
        .await
        .unwrap()
        .into();

    // Authenticate all accounts
    let mut john_client = test_account_login("jdoe@example.com", "12345").await;
    let mut jane_client = test_account_login("jane.smith@example.com", "abcde").await;
    let mut bill_client = test_account_login("bill@example.com", "098765").await;

    // Insert two emails in each account
    let mut email_ids = AHashMap::default();
    for (client, account_id, name) in [
        (&mut john_client, &john_id, "john"),
        (&mut jane_client, &jane_id, "jane"),
        (&mut bill_client, &bill_id, "bill"),
        (admin_client, &sales_id, "sales"),
    ] {
        let user_name = client.session().username().to_string();
        let mut ids = Vec::with_capacity(2);
        for (mailbox_id, mailbox_name) in [(&inbox_id, "inbox"), (&trash_id, "trash")] {
            ids.push(
                client
                    .set_default_account_id(account_id.to_string())
                    .email_import(
                        format!(
                            concat!(
                                "From: acl_test@example.com\r\n",
                                "To: {}\r\n",
                                "Subject: Owned by {} in {}\r\n",
                                "\r\n",
                                "This message is owned by {}.",
                            ),
                            user_name, name, mailbox_name, name
                        )
                        .into_bytes(),
                        [mailbox_id],
                        None::<Vec<&str>>,
                        None,
                    )
                    .await
                    .unwrap()
                    .take_id(),
            );
        }
        email_ids.insert(name, ids);
    }

    // John should have access to his emails only
    assert_eq!(
        john_client
            .email_get(
                email_ids.get("john").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Owned by john in inbox"
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_get(
                email_ids.get("jane").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await,
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_get(&inbox_id, None::<Vec<_>>)
            .await,
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&sales_id.to_string())
            .email_get(
                email_ids.get("sales").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await,
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&sales_id.to_string())
            .mailbox_get(&inbox_id, None::<Vec<_>>)
            .await,
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_query(None::<Filter>, None::<Vec<_>>)
            .await,
    );

    // Jane grants Inbox ReadItems access to John
    jane_client
        .mailbox_update_acl(&inbox_id, "jdoe@example.com", [ACL::ReadItems])
        .await
        .unwrap();

    // John shoud have ReadItems access to Inbox
    assert_eq!(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_get(
                email_ids.get("jane").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Owned by jane in inbox"
    );
    assert_eq!(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_query(None::<Filter>, None::<Vec<_>>)
            .await
            .unwrap()
            .ids(),
        [email_ids.get("jane").unwrap().first().unwrap().as_str()]
    );

    // John's session resource should contain Jane's account details
    john_client.refresh_session().await.unwrap();
    assert_eq!(
        john_client
            .session()
            .account(&jane_id.to_string())
            .unwrap()
            .name(),
        "jane.smith@example.com"
    );

    // John should not have access to emails in Jane's Trash folder
    assert!(john_client
        .set_default_account_id(&jane_id.to_string())
        .email_get(
            email_ids.get("jane").unwrap().last().unwrap(),
            [Property::Subject].into(),
        )
        .await
        .unwrap()
        .is_none());

    // John should only be able to copy blobs he has access to
    let blob_id = jane_client
        .email_get(
            email_ids.get("jane").unwrap().first().unwrap(),
            [Property::BlobId].into(),
        )
        .await
        .unwrap()
        .unwrap()
        .take_blob_id();
    john_client
        .set_default_account_id(&john_id.to_string())
        .blob_copy(&jane_id.to_string(), &blob_id)
        .await
        .unwrap();
    let blob_id = jane_client
        .email_get(
            email_ids.get("jane").unwrap().last().unwrap(),
            [Property::BlobId].into(),
        )
        .await
        .unwrap()
        .unwrap()
        .take_blob_id();
    assert_forbidden(
        john_client
            .set_default_account_id(&john_id.to_string())
            .blob_copy(&jane_id.to_string(), &blob_id)
            .await,
    );

    // John only has ReadItems access to Inbox but no Read access
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_get(&inbox_id, [mailbox::Property::MyRights].into())
            .await,
    );
    jane_client
        .mailbox_update_acl(&inbox_id, "jdoe@example.com", [ACL::Read, ACL::ReadItems])
        .await
        .unwrap();
    assert_eq!(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_get(&inbox_id, [mailbox::Property::MyRights].into())
            .await
            .unwrap()
            .unwrap()
            .my_rights()
            .unwrap()
            .acl_list(),
        vec![ACL::ReadItems]
    );

    // Try to add items using import and copy
    let blob_id = john_client
        .set_default_account_id(&john_id.to_string())
        .upload(
            Some(&john_id.to_string()),
            concat!(
                "From: acl_test@example.com\r\n",
                "To: jane.smith@example.com\r\n",
                "Subject: Created by john in jane's inbox\r\n",
                "\r\n",
                "This message is owned by jane.",
            )
            .as_bytes()
            .to_vec(),
            None,
        )
        .await
        .unwrap()
        .take_blob_id();
    let mut request = john_client
        .set_default_account_id(&jane_id.to_string())
        .build();
    let email_id = request
        .import_email()
        .email(&blob_id)
        .mailbox_ids([&inbox_id])
        .create_id();
    assert_forbidden(
        request
            .send_single::<EmailImportResponse>()
            .await
            .unwrap()
            .created(&email_id),
    );
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_copy(
                &john_id.to_string(),
                email_ids.get("john").unwrap().last().unwrap(),
                [&inbox_id],
                None::<Vec<&str>>,
                None,
            )
            .await,
    );

    // Grant access and try again
    jane_client
        .mailbox_update_acl(
            &inbox_id,
            "jdoe@example.com",
            [ACL::Read, ACL::ReadItems, ACL::AddItems],
        )
        .await
        .unwrap();

    let mut request = john_client
        .set_default_account_id(&jane_id.to_string())
        .build();
    let email_id = request
        .import_email()
        .email(&blob_id)
        .mailbox_ids([&inbox_id])
        .create_id();
    let email_id = request
        .send_single::<EmailImportResponse>()
        .await
        .unwrap()
        .created(&email_id)
        .unwrap()
        .take_id();
    let email_id_2 = john_client
        .set_default_account_id(&jane_id.to_string())
        .email_copy(
            &john_id.to_string(),
            email_ids.get("john").unwrap().last().unwrap(),
            [&inbox_id],
            None::<Vec<&str>>,
            None,
        )
        .await
        .unwrap()
        .take_id();

    assert_eq!(
        jane_client
            .email_get(&email_id, [Property::Subject].into(),)
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Created by john in jane's inbox"
    );
    assert_eq!(
        jane_client
            .email_get(&email_id_2, [Property::Subject].into(),)
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Owned by john in trash"
    );

    // Try removing items
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_destroy(&email_id)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &inbox_id,
            "jdoe@example.com",
            [ACL::Read, ACL::ReadItems, ACL::AddItems, ACL::RemoveItems],
        )
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .email_destroy(&email_id)
        .await
        .unwrap();

    // Try to set keywords
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_set_keyword(&email_id_2, "$seen", true)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &inbox_id,
            "jdoe@example.com",
            [
                ACL::Read,
                ACL::ReadItems,
                ACL::AddItems,
                ACL::RemoveItems,
                ACL::ModifyItems,
            ],
        )
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .email_set_keyword(&email_id_2, "$seen", true)
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .email_set_keyword(&email_id_2, "my-keyword", true)
        .await
        .unwrap();

    // Try to create a child
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_create("John's mailbox", None::<&str>, Role::None)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &inbox_id,
            "jdoe@example.com",
            [
                ACL::Read,
                ACL::ReadItems,
                ACL::AddItems,
                ACL::RemoveItems,
                ACL::ModifyItems,
                ACL::CreateChild,
            ],
        )
        .await
        .unwrap();
    let mailbox_id = john_client
        .set_default_account_id(&jane_id.to_string())
        .mailbox_create("John's mailbox", Some(&inbox_id), Role::None)
        .await
        .unwrap()
        .take_id();

    // Try renaming a mailbox
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_rename(&mailbox_id, "John's private mailbox")
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &mailbox_id,
            "jdoe@example.com",
            [ACL::Read, ACL::ReadItems, ACL::Modify],
        )
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .mailbox_rename(&mailbox_id, "John's private mailbox")
        .await
        .unwrap();

    // Try moving a message
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_set_mailbox(&email_id_2, &mailbox_id, true)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &mailbox_id,
            "jdoe@example.com",
            [ACL::Read, ACL::ReadItems, ACL::Modify, ACL::AddItems],
        )
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .email_set_mailbox(&email_id_2, &mailbox_id, true)
        .await
        .unwrap();

    // Try deleting a mailbox
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_destroy(&mailbox_id, true)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &mailbox_id,
            "jdoe@example.com",
            [
                ACL::Read,
                ACL::ReadItems,
                ACL::Modify,
                ACL::AddItems,
                ACL::Delete,
            ],
        )
        .await
        .unwrap();
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_destroy(&mailbox_id, true)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &mailbox_id,
            "jdoe@example.com",
            [
                ACL::Read,
                ACL::ReadItems,
                ACL::Modify,
                ACL::AddItems,
                ACL::Delete,
                ACL::RemoveItems,
            ],
        )
        .await
        .unwrap();
    john_client
        .set_default_account_id(&jane_id.to_string())
        .mailbox_destroy(&mailbox_id, true)
        .await
        .unwrap();

    // Try changing ACL
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_update_acl(&inbox_id, "bill@example.com", [ACL::Read, ACL::ReadItems])
            .await,
    );
    assert_forbidden(
        bill_client
            .set_default_account_id(&jane_id.to_string())
            .email_query(None::<Filter>, None::<Vec<_>>)
            .await,
    );
    jane_client
        .mailbox_update_acl(
            &inbox_id,
            "jdoe@example.com",
            [
                ACL::Read,
                ACL::ReadItems,
                ACL::AddItems,
                ACL::RemoveItems,
                ACL::ModifyItems,
                ACL::CreateChild,
                ACL::Modify,
                ACL::Administer,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .mailbox_get(&inbox_id, [mailbox::Property::MyRights].into())
            .await
            .unwrap()
            .unwrap()
            .my_rights()
            .unwrap()
            .acl_list(),
        vec![
            ACL::ReadItems,
            ACL::AddItems,
            ACL::RemoveItems,
            ACL::ModifyItems,
            ACL::CreateChild,
            ACL::Modify
        ]
    );
    john_client
        .set_default_account_id(&jane_id.to_string())
        .mailbox_update_acl(&inbox_id, "bill@example.com", [ACL::Read, ACL::ReadItems])
        .await
        .unwrap();
    assert_eq!(
        bill_client
            .set_default_account_id(&jane_id.to_string())
            .email_query(
                None::<Filter>,
                vec![email::query::Comparator::subject()].into()
            )
            .await
            .unwrap()
            .ids(),
        [
            email_ids.get("jane").unwrap().first().unwrap().as_str(),
            &email_id_2
        ]
    );

    // Revoke all access to John
    jane_client
        .mailbox_update_acl(&inbox_id, "jdoe@example.com", [])
        .await
        .unwrap();
    assert_forbidden(
        john_client
            .set_default_account_id(&jane_id.to_string())
            .email_get(
                email_ids.get("jane").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await,
    );
    john_client.refresh_session().await.unwrap();
    assert!(john_client
        .session()
        .account(&jane_id.to_string())
        .is_none());
    assert_eq!(
        bill_client
            .set_default_account_id(&jane_id.to_string())
            .email_get(
                email_ids.get("jane").unwrap().first().unwrap(),
                [Property::Subject].into(),
            )
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Owned by jane in inbox"
    );

    // Add John and Jane to the Sales group
    for name in ["jdoe@example.com", "jane.smith@example.com"] {
        add_to_group(directory, name, "sales@example.com").await;
    }
    server.access_tokens.clear();
    john_client.refresh_session().await.unwrap();
    jane_client.refresh_session().await.unwrap();
    bill_client.refresh_session().await.unwrap();
    assert_eq!(
        john_client
            .session()
            .account(&sales_id.to_string())
            .unwrap()
            .name(),
        "sales@example.com"
    );
    assert!(!john_client
        .session()
        .account(&sales_id.to_string())
        .unwrap()
        .is_personal());
    assert_eq!(
        jane_client
            .session()
            .account(&sales_id.to_string())
            .unwrap()
            .name(),
        "sales@example.com"
    );
    assert!(bill_client
        .session()
        .account(&sales_id.to_string())
        .is_none());

    // Insert a message in Sales's inbox
    let blob_id = john_client
        .set_default_account_id(&sales_id.to_string())
        .upload(
            Some(&sales_id.to_string()),
            concat!(
                "From: acl_test@example.com\r\n",
                "To: sales@example.com\r\n",
                "Subject: Created by john in sales\r\n",
                "\r\n",
                "This message is owned by sales.",
            )
            .as_bytes()
            .to_vec(),
            None,
        )
        .await
        .unwrap()
        .take_blob_id();
    let mut request = john_client.build();
    let email_id = request
        .import_email()
        .email(&blob_id)
        .mailbox_ids([&inbox_id])
        .create_id();
    let email_id = request
        .send_single::<EmailImportResponse>()
        .await
        .unwrap()
        .created(&email_id)
        .unwrap()
        .take_id();

    // Both Jane and John should be able to see this message, but not Bill
    assert_eq!(
        john_client
            .set_default_account_id(&sales_id.to_string())
            .email_get(&email_id, [Property::Subject].into(),)
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Created by john in sales"
    );
    assert_eq!(
        jane_client
            .set_default_account_id(&sales_id.to_string())
            .email_get(&email_id, [Property::Subject].into(),)
            .await
            .unwrap()
            .unwrap()
            .subject()
            .unwrap(),
        "Created by john in sales"
    );
    assert_forbidden(
        bill_client
            .set_default_account_id(&sales_id.to_string())
            .email_get(&email_id, [Property::Subject].into())
            .await,
    );

    // Remove John from the sales group
    remove_from_group(directory, "jdoe@example.com", "sales@example.com").await;
    server.sessions.clear();
    assert_forbidden(
        john_client
            .set_default_account_id(&sales_id.to_string())
            .email_get(&email_id, [Property::Subject].into())
            .await,
    );

    // Destroy test account data
    for id in [john_id, bill_id, jane_id, sales_id] {
        admin_client.set_default_account_id(&id.to_string());
        destroy_all_mailboxes(admin_client).await;
    }
    server.store.assert_is_empty().await;
}

pub fn assert_forbidden<T: Debug>(result: Result<T, jmap_client::Error>) {
    if !matches!(
        result,
        Err(jmap_client::Error::Method(MethodError {
            p_type: MethodErrorType::Forbidden
        })) | Err(jmap_client::Error::Set(SetError {
            type_: SetErrorType::Forbidden,
            ..
        }))
    ) {
        panic!("Expected forbidden, got {:?}", result);
    }
}
