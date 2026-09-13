//! Searching a channel or a server.
//!
//! Two rules, and the second is the one that would be easy to get wrong.
//!
//! **Searches are kept apart in time.** A search overlay is a text box somebody
//! types into, and one request per keystroke is what a bot looks like. The same
//! [`Spacer`](crate::discord::gifs::Spacer) the GIF picker uses runs the gate
//! here: three hundred milliseconds between requests, and a request overtaken
//! while it waits is dropped rather than sent.
//!
//! **Results never reach the message store.** A search reaches back through a
//! year of a channel nobody has open, and putting what comes back into the
//! store would throw away the window somebody is reading and make what is on
//! screen depend on what was last searched for. So the page is carried on the
//! event — the one place an `Event` in this program carries data rather than
//! pointing at `State` — and jumping to a result is an ordinary
//! `Command::JumpTo`, which fetches the page around it properly.

use crate::discord::gifs::{Slot, SPACING};
use crate::discord::handle::{Event, RequestId, SearchQuery, SearchScope};
use crate::discord::http::api;
use crate::discord::http::route::{SearchIn, SearchTerms};

use super::Ops;

/// `Command::Search`.
pub async fn search(ops: &Ops, id: RequestId, scope: SearchScope, query: SearchQuery) {
    let slot = {
        let mut shared = ops.shared();
        shared
            .searches
            .claim(id, std::time::Instant::now(), SPACING)
    };
    match slot {
        Slot::Now => {}
        Slot::Superseded => {
            tracing::trace!("search {} was overtaken before it ran", id.0);
            return;
        }
        Slot::After(wait) => {
            tokio::time::sleep(wait).await;
            if !ops.shared().searches.is_current(id) {
                tracing::trace!("search {} was overtaken while it waited", id.0);
                return;
            }
        }
    }

    let scope = match scope {
        SearchScope::Guild(guild) => SearchIn::Guild(guild),
        SearchScope::Channel(channel) => SearchIn::Channel(channel),
    };
    let terms = SearchTerms {
        content: query.content,
        channel: query.channel,
        author: query.author,
        offset: query.offset,
    };

    let result = ops
        .rest(api::search(&ops.http, scope, &terms))
        .await
        .map_err(|e| {
            tracing::debug!("the search came back with nothing: {e}");
            e.to_string()
        });

    ops.emit(Event::Search { id, result });
}

#[cfg(test)]
mod tests {
    use super::super::testing::harness;
    use super::*;
    use crate::discord::handle::SearchPage;
    use crate::discord::snowflake::{ChannelId, GuildId, MessageId};

    fn message(id: u64, content: &str, hit: bool) -> serde_json::Value {
        serde_json::json!({
            "id": id.to_string(),
            "channel_id": "7",
            "content": content,
            "hit": hit,
            "author": {"id": "2", "username": "alex"}
        })
    }

    fn page(events: &crossbeam_channel::Receiver<Event>) -> Result<SearchPage, String> {
        for event in events.try_iter() {
            if let Event::Search { result, .. } = event {
                return result;
            }
        }
        panic!("nothing answered the search");
    }

    #[test]
    fn a_guild_search_carries_its_filters_and_a_channel_search_does_not() {
        use crate::discord::http::route::Route;

        let terms = SearchTerms {
            content: "hello world".into(),
            channel: Some(ChannelId(7)),
            author: Some(crate::discord::snowflake::UserId(2)),
            offset: 50,
        };
        let guild = Route::Search {
            scope: SearchIn::Guild(GuildId(9)),
            query: terms.clone(),
        };
        assert_eq!(
            guild.path(),
            "/guilds/9/messages/search?content=hello%20world&channel_id=7&author_id=2&offset=50"
        );

        let channel = Route::Search {
            scope: SearchIn::Channel(ChannelId(7)),
            query: terms,
        };
        assert_eq!(
            channel.path(),
            "/channels/7/messages/search?content=hello%20world&author_id=2&offset=50",
            "a channel search is already narrowed to a channel"
        );
    }

    /// The words are not part of the allowance; the guild is.
    #[test]
    fn two_searches_of_one_guild_share_an_allowance() {
        use crate::discord::http::route::Route;

        let one = Route::Search {
            scope: SearchIn::Guild(GuildId(9)),
            query: SearchTerms {
                content: "a".into(),
                ..Default::default()
            },
        };
        let two = Route::Search {
            scope: SearchIn::Guild(GuildId(9)),
            query: SearchTerms {
                content: "b".into(),
                offset: 25,
                ..Default::default()
            },
        };
        assert_eq!(one.bucket(), two.bucket());
        assert_ne!(
            one.bucket(),
            Route::Search {
                scope: SearchIn::Guild(GuildId(10)),
                query: SearchTerms::default(),
            }
            .bucket()
        );
    }

    #[tokio::test]
    async fn a_search_returns_the_hit_out_of_each_group_of_context() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/guilds/9/messages/search"))
            .and(query_param("content", "kettle"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_results": 42,
                "messages": [
                    [
                        message(100, "before", false),
                        message(101, "the kettle is on", true),
                        message(102, "after", false)
                    ],
                    [message(200, "another kettle", true)]
                ]
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        search(
            &h.ops,
            RequestId(1),
            SearchScope::Guild(GuildId(9)),
            SearchQuery {
                content: "kettle".into(),
                ..Default::default()
            },
        )
        .await;

        let page = page(&h.events).expect("the search failed");
        assert_eq!(page.total, 42, "the total is the whole count, not the page");
        assert_eq!(page.offset, 0);
        assert_eq!(
            page.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![MessageId(101), MessageId(200)],
            "the context around a hit is not a result"
        );

        // And none of it reached the store.
        assert!(
            h.ops.state().messages(ChannelId(7)).is_none(),
            "search results must not become a channel's window"
        );
    }

    #[tokio::test]
    async fn the_next_page_asks_for_the_offset_after_this_one() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("offset", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_results": 42,
                "messages": [[message(300, "page two", true)]]
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        search(
            &h.ops,
            RequestId(1),
            SearchScope::Channel(ChannelId(7)),
            SearchQuery {
                content: "kettle".into(),
                offset: 25,
                ..Default::default()
            },
        )
        .await;

        let page = page(&h.events).expect("the search failed");
        assert_eq!(page.offset, 25);
        assert_eq!(page.messages.len(), 1);
    }

    /// A guild Discord has not finished indexing answers 202 with a wait rather
    /// than results, which is a success status carrying a failure.
    #[tokio::test]
    async fn a_guild_still_being_indexed_says_so_rather_than_looking_empty() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "code": 110000,
                "retry_after": 3,
                "documents_indexed": 1200
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        search(
            &h.ops,
            RequestId(1),
            SearchScope::Guild(GuildId(9)),
            SearchQuery::default(),
        )
        .await;

        let reason = page(&h.events).expect_err("an unindexed guild is not an empty result");
        assert!(reason.contains("indexing"), "{reason}");
    }

    /// The same gate the GIF picker uses, on the same reasoning: a search box
    /// somebody types into must not be one request per letter.
    #[tokio::test]
    async fn a_search_overtaken_while_it_waited_never_runs() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total_results": 0, "messages": []
            })))
            .mount(&server)
            .await;

        let h = harness(&server.uri());
        // Claim a later id first, as a second keystroke would.
        h.ops
            .shared()
            .searches
            .claim(RequestId(5), std::time::Instant::now(), SPACING);

        search(
            &h.ops,
            RequestId(2),
            SearchScope::Channel(ChannelId(7)),
            SearchQuery::default(),
        )
        .await;

        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a search nobody is waiting for was sent anyway"
        );
        assert!(
            h.events.try_iter().next().is_none(),
            "and it answered one nobody asked"
        );
    }
}
