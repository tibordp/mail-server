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

use std::{sync::Arc, time::Duration};

use futures::future::join_all;
use jmap::JMAP;
use jmap_client::{
    client::Client,
    core::set::{SetErrorType, SetObject},
    mailbox::{self, Mailbox, Role},
};
use jmap_proto::types::{collection::Collection, id::Id, property::Property};
use store::rand::{self, Rng};

use crate::jmap::mailbox::destroy_all_mailboxes;

const TEST_USER_ID: u32 = 1;
const NUM_PASSES: usize = 1;

pub async fn test(server: Arc<JMAP>, mut client: Client) {
    println!("Running concurrency stress tests...");

    client.set_default_account_id(Id::from(TEST_USER_ID).to_string());
    let client = Arc::new(client);

    email_tests(server.clone(), client.clone()).await;
    mailbox_tests(server.clone(), client.clone()).await;
}

async fn email_tests(server: Arc<JMAP>, client: Arc<Client>) {
    for pass in 0..NUM_PASSES {
        println!("----------------- PASS {} -----------------", pass);
        let mailboxes = Arc::new(vec![
            client
                .mailbox_create("Stress 1", None::<String>, Role::None)
                .await
                .unwrap()
                .take_id(),
            client
                .mailbox_create("Stress 2", None::<String>, Role::None)
                .await
                .unwrap()
                .take_id(),
            client
                .mailbox_create("Stress 3", None::<String>, Role::None)
                .await
                .unwrap()
                .take_id(),
        ]);
        let mut futures = Vec::new();

        for num in 0..1000 {
            match rand::thread_rng().gen_range(0..3) {
                0 => {
                    let client = client.clone();
                    let mailboxes = mailboxes.clone();
                    futures.push(tokio::spawn(async move {
                        let mailbox_num =
                            rand::thread_rng().gen_range::<usize, _>(0..mailboxes.len());
                        let _message_id = client
                            .email_import(
                                format!(
                                    concat!(
                                        "From: test@test.com\n",
                                        "To: test@test.com\r\n",
                                        "Subject: test {}\r\n\r\ntest {}\r\n"
                                    ),
                                    num, num
                                )
                                .into_bytes(),
                                [&mailboxes[mailbox_num]],
                                None::<Vec<String>>,
                                None,
                            )
                            .await
                            .unwrap()
                            .take_id();
                        //println!("Inserted message {}.", message_id);
                    }));
                }

                1 => {
                    let client = client.clone();
                    futures.push(tokio::spawn(async move {
                        loop {
                            let mut req = client.build();
                            req.query_email();
                            let ids = req.send_query_email().await.unwrap().take_ids();
                            if !ids.is_empty() {
                                let message_id = &ids[rand::thread_rng().gen_range(0..ids.len())];
                                //println!("Deleting message {}.", message_id);
                                match client.email_destroy(message_id).await {
                                    Ok(_) => {
                                        break;
                                    }
                                    Err(jmap_client::Error::Set(err)) => match err.error() {
                                        SetErrorType::NotFound => {
                                            break;
                                        }
                                        SetErrorType::Forbidden => {
                                            // Concurrency issue, try again.
                                            println!("Concurrent update, trying again.");
                                        }
                                        _ => {
                                            panic!("Unexpected error: {:?}", err);
                                        }
                                    },
                                    Err(err) => {
                                        panic!("Unexpected error: {:?}", err);
                                    }
                                }
                            } else {
                                break;
                            }
                        }
                    }));
                }
                _ => {
                    let client = client.clone();
                    let mailboxes = mailboxes.clone();
                    futures.push(tokio::spawn(async move {
                        let mut req = client.build();
                        let ref_id = req.query_email().result_reference();
                        req.get_email()
                            .ids_ref(ref_id)
                            .properties([jmap_client::email::Property::MailboxIds]);
                        let emails = req
                            .send()
                            .await
                            .unwrap()
                            .unwrap_method_responses()
                            .pop()
                            .unwrap()
                            .unwrap_get_email()
                            .unwrap()
                            .take_list();

                        if !emails.is_empty() {
                            let message = &emails[rand::thread_rng().gen_range(0..emails.len())];
                            let message_id = message.id().unwrap();
                            let mailbox_ids = message.mailbox_ids();
                            assert_eq!(mailbox_ids.len(), 1, "{:#?}", message);
                            let mailbox_id = mailbox_ids.last().unwrap();
                            loop {
                                let new_mailbox_id =
                                    &mailboxes[rand::thread_rng().gen_range(0..mailboxes.len())];
                                if new_mailbox_id != mailbox_id {
                                    /*println!(
                                        "Moving message {} from {} to {}.",
                                        message_id, mailbox_id, new_mailbox_id
                                    );*/
                                    let mut req = client.build();
                                    req.set_email()
                                        .update(message_id)
                                        .mailbox_ids([new_mailbox_id]);
                                    req.send_set_email().await.unwrap();

                                    break;
                                }
                            }
                        }
                    }));
                }
            }
            tokio::time::sleep(Duration::from_millis(rand::thread_rng().gen_range(5..10))).await;
        }

        join_all(futures).await;

        let email_ids = server
            .get_document_ids(TEST_USER_ID, Collection::Email)
            .await
            .unwrap()
            .unwrap_or_default();
        let mailbox_ids = server
            .get_document_ids(TEST_USER_ID, Collection::Mailbox)
            .await
            .unwrap()
            .unwrap_or_default();
        assert_eq!(mailbox_ids.len(), 8);

        for mailbox in mailboxes.iter() {
            let mailbox_id = Id::from_bytes(mailbox.as_bytes()).unwrap().document_id();
            let email_ids_in_mailbox = server
                .get_tag(
                    TEST_USER_ID,
                    Collection::Email,
                    Property::MailboxIds,
                    mailbox_id,
                )
                .await
                .unwrap()
                .unwrap_or_default();
            let mut email_ids_check = email_ids_in_mailbox.clone();
            email_ids_check &= &email_ids;
            assert_eq!(email_ids_in_mailbox, email_ids_check);

            //println!("Emails {:?}", email_ids_in_mailbox);

            for email_id in &email_ids_in_mailbox {
                if let Some(mailbox_tags) = server
                    .get_property::<Vec<u32>>(
                        TEST_USER_ID,
                        Collection::Email,
                        email_id,
                        &Property::MailboxIds,
                    )
                    .await
                    .unwrap()
                {
                    if mailbox_tags.len() != 1 {
                        panic!(
                        "Email ORM has more than one mailbox {:?}! Id {} in mailbox {} with messages {:?}",
                        mailbox_tags, email_id, mailbox_id, email_ids_in_mailbox
                    );
                    }
                    let mailbox_tag = mailbox_tags[0];
                    if mailbox_tag != mailbox_id {
                        panic!(
                            concat!(
                                "Email ORM has an unexpected mailbox tag {}! Id {} in ",
                                "mailbox {} with messages {:?}"
                            ),
                            mailbox_tag, email_id, mailbox_id, email_ids_in_mailbox,
                        );
                    }
                } else {
                    panic!(
                        "Email tags not found! Id {} in mailbox {} with messages {:?}",
                        email_id, mailbox_id, email_ids_in_mailbox
                    );
                }
            }
        }

        destroy_all_mailboxes(&client).await;

        server.store.assert_is_empty().await;
    }
}

async fn mailbox_tests(server: Arc<JMAP>, client: Arc<Client>) {
    let mailboxes = Arc::new(vec![
        "test/test1/test2/test3".to_string(),
        "test1/test2/test3".to_string(),
        "test2/test3/test4".to_string(),
        "test3/test4/test5".to_string(),
        "test4".to_string(),
        "test5".to_string(),
    ]);
    let mut futures = Vec::new();

    for _ in 0..1000 {
        match rand::thread_rng().gen_range(0..=3) {
            0 => {
                for pos in 0..mailboxes.len() {
                    let client = client.clone();
                    let mailboxes = mailboxes.clone();
                    futures.push(tokio::spawn(async move {
                        create_mailbox(&client, &mailboxes[pos]).await;
                    }));
                }
            }

            1 => {
                let client = client.clone();
                futures.push(tokio::spawn(async move {
                    query_mailboxes(&client).await;
                }));
            }

            2 => {
                let client = client.clone();
                futures.push(tokio::spawn(async move {
                    for mailbox_id in client
                        .mailbox_query(None::<mailbox::query::Filter>, None::<Vec<_>>)
                        .await
                        .unwrap()
                        .take_ids()
                    {
                        let client = client.clone();
                        tokio::spawn(async move {
                            delete_mailbox(&client, &mailbox_id).await;
                        });
                    }
                }));
            }

            _ => {
                let client = client.clone();
                futures.push(tokio::spawn(async move {
                    let mut ids = client
                        .mailbox_query(None::<mailbox::query::Filter>, None::<Vec<_>>)
                        .await
                        .unwrap()
                        .take_ids();
                    if !ids.is_empty() {
                        let id = ids.swap_remove(rand::thread_rng().gen_range(0..ids.len()));
                        let sort_order = rand::thread_rng().gen_range(0..100);
                        client.mailbox_update_sort_order(&id, sort_order).await.ok();
                    }
                }));
            }
        }
        tokio::time::sleep(Duration::from_millis(rand::thread_rng().gen_range(5..10))).await;
    }

    join_all(futures).await;

    destroy_all_mailboxes(&client).await;
    server.store.assert_is_empty().await;
}

async fn create_mailbox(client: &Client, mailbox: &str) -> Vec<String> {
    let mut request = client.build();
    let mut create_ids: Vec<String> = Vec::new();
    let set_request = request.set_mailbox();
    for path_item in mailbox.split('/') {
        let create_item = set_request.create().name(path_item);
        if let Some(create_id) = create_ids.last() {
            create_item.parent_id_ref(create_id);
        }
        create_ids.push(create_item.create_id().unwrap());
    }
    let mut response = request.send_set_mailbox().await.unwrap();
    let mut ids = Vec::with_capacity(create_ids.len());
    for create_id in create_ids {
        if let Ok(mut id) = response.created(&create_id) {
            ids.push(id.take_id());
        }
    }
    ids
}

async fn query_mailboxes(client: &Client) -> Vec<Mailbox> {
    let mut request = client.build();
    let query_result = request
        .query_mailbox()
        .calculate_total(true)
        .result_reference();
    request.get_mailbox().ids_ref(query_result).properties([
        jmap_client::mailbox::Property::Id,
        jmap_client::mailbox::Property::Name,
        jmap_client::mailbox::Property::IsSubscribed,
        jmap_client::mailbox::Property::ParentId,
        jmap_client::mailbox::Property::Role,
        jmap_client::mailbox::Property::TotalEmails,
        jmap_client::mailbox::Property::UnreadEmails,
    ]);

    request
        .send()
        .await
        .unwrap()
        .unwrap_method_responses()
        .pop()
        .unwrap()
        .unwrap_get_mailbox()
        .unwrap()
        .take_list()
}

async fn delete_mailbox(client: &Client, mailbox_id: &str) {
    match client.mailbox_destroy(mailbox_id, true).await {
        Ok(_) => (),
        Err(err) => match err {
            jmap_client::Error::Set(_) => (),
            _ => panic!("Failed: {:?}", err),
        },
    }
}
