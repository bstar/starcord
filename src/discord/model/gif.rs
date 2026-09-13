//! What the GIF picker gets back.
//!
//! Discord proxies somebody else's GIF service rather than hosting the results
//! itself, and which service that is has changed before and is changing again:
//! Tenor today, Giphy and Klipy announced for 2026. So the provider is a string
//! in the request rather than a constant in the code, and everything here is
//! `#[serde(default)]` — a result missing a `preview` is a result to draw
//! without one, not a picker that fails to open.
//!
//! Two response shapes are accepted for trending. A bare array is what the
//! search endpoint returns and what trending returned historically; the current
//! client's trending call comes back as `{categories, gifs}`, which is the same
//! list with a row of category tiles in front of it. Both are read here, as the
//! rest of `model/` reads both shapes of everything else Discord has changed
//! its mind about.

use serde::{Deserialize, Serialize};

/// One GIF, as the picker draws it.
///
/// `url` is what gets *sent*: posting a GIF is an ordinary message whose whole
/// content is that link, which Discord then unfurls into a `gifv` embed. `src`
/// and `gif_src` are the picture itself in two formats — an mp4 and a real GIF
/// — and `preview` is the still that fills the tile until one of them arrives.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GifResult {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// The page to post. Everything else here is a picture; this is the link.
    #[serde(default)]
    pub url: String,
    /// Usually an mp4, which is smaller than the GIF by an order of magnitude
    /// and which this client never decodes.
    #[serde(default)]
    pub src: String,
    /// The animated GIF, which is the one a terminal can show.
    #[serde(default)]
    pub gif_src: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// A still frame, for the tile before anything has been fetched.
    #[serde(default)]
    pub preview: String,
}

impl GifResult {
    /// The best picture to draw in a grid tile.
    ///
    /// The preview first, because it is one frame rather than a hundred and a
    /// picker shows twenty tiles at once. The animated source is the fallback,
    /// not the default.
    pub fn tile(&self) -> Option<&str> {
        [&self.preview, &self.gif_src, &self.src]
            .into_iter()
            .map(String::as_str)
            .find(|candidate| !candidate.is_empty())
    }

    /// Whether this is worth showing at all. A result with no link cannot be
    /// posted, whatever else it carries.
    pub fn is_postable(&self) -> bool {
        !self.url.is_empty()
    }
}

/// A trending category, which the picker may show as a row of shortcuts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GifCategory {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub src: String,
}

/// The trending response, in whichever shape arrived.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Trending {
    /// `{categories, gifs}`, which is what the current web client receives.
    Sectioned {
        #[serde(default)]
        categories: Vec<GifCategory>,
        #[serde(default)]
        gifs: Vec<GifResult>,
    },
    /// A bare array, which is what search returns and what trending used to.
    Bare(Vec<GifResult>),
}

impl Trending {
    pub fn into_results(self) -> Vec<GifResult> {
        match self {
            Trending::Sectioned { gifs, .. } => gifs,
            Trending::Bare(gifs) => gifs,
        }
    }

    pub fn categories(&self) -> &[GifCategory] {
        match self {
            Trending::Sectioned { categories, .. } => categories,
            Trending::Bare(_) => &[],
        }
    }
}

/// What `/gifs/suggest` answers with.
///
/// Documented as an array of strings and observed, on some accounts, as an
/// array of objects carrying a `query`. Both are read, because a completion
/// list that fails to parse should cost the completions and not the picker.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Suggestion {
    Text(String),
    Object {
        #[serde(default)]
        query: String,
        #[serde(default)]
        name: String,
    },
}

impl Suggestion {
    pub fn into_text(self) -> String {
        match self {
            Suggestion::Text(text) => text,
            Suggestion::Object { query, name } => {
                if query.is_empty() {
                    name
                } else {
                    query
                }
            }
        }
    }
}

/// One answer from the picker's three requests.
///
/// Trending and search fill `results`; suggest fills `suggestions`. One type
/// rather than three because [`crate::discord::handle::Event::Gifs`] carries
/// whichever of them the request asked for, and a UI that has to match on which
/// request it made to know what it got is a UI keeping the core's books for it.
#[derive(Debug, Clone, Default)]
pub struct GifPage {
    pub results: Vec<GifResult>,
    pub categories: Vec<GifCategory>,
    /// Search terms, from `suggest`. Never links.
    pub suggestions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_result_carries_a_link_to_post_and_a_picture_to_draw() {
        let results: Vec<GifResult> = serde_json::from_str(
            r#"[{"id":"123","title":"a cat","url":"https://tenor.com/view/cat-123",
                 "src":"https://media.tenor.com/x.mp4","gif_src":"https://media.tenor.com/x.gif",
                 "width":498,"height":280,"preview":"https://media.tenor.com/x.png"}]"#,
        )
        .unwrap();

        let gif = &results[0];
        assert_eq!(gif.url, "https://tenor.com/view/cat-123");
        assert_eq!(gif.tile(), Some("https://media.tenor.com/x.png"));
        assert_eq!((gif.width, gif.height), (498, 280));
        assert!(gif.is_postable());
    }

    /// Every field is optional, because a provider that adds one is a provider
    /// that has also dropped one before now.
    #[test]
    fn a_result_missing_everything_is_still_a_result() {
        let gif: GifResult = serde_json::from_str("{}").unwrap();
        assert_eq!(gif.tile(), None);
        assert!(!gif.is_postable(), "there is nothing to send");
    }

    #[test]
    fn a_tile_falls_back_through_the_pictures_it_has() {
        let animated = GifResult {
            gif_src: "https://x/a.gif".into(),
            ..Default::default()
        };
        assert_eq!(animated.tile(), Some("https://x/a.gif"));

        let video_only = GifResult {
            src: "https://x/a.mp4".into(),
            ..Default::default()
        };
        assert_eq!(video_only.tile(), Some("https://x/a.mp4"));
    }

    #[test]
    fn trending_is_read_as_a_bare_array_or_as_sections() {
        let bare: Trending = serde_json::from_str(r#"[{"id":"1","url":"https://a"}]"#).unwrap();
        assert!(bare.categories().is_empty());
        assert_eq!(bare.into_results().len(), 1);

        let sectioned: Trending = serde_json::from_str(
            r#"{"categories":[{"name":"reaction","src":"https://c.png"}],
                "gifs":[{"id":"1","url":"https://a"},{"id":"2","url":"https://b"}]}"#,
        )
        .unwrap();
        assert_eq!(sectioned.categories().len(), 1);
        assert_eq!(sectioned.categories()[0].name, "reaction");
        assert_eq!(sectioned.into_results().len(), 2);
    }

    #[test]
    fn a_suggestion_is_read_from_either_spelling() {
        let strings: Vec<Suggestion> = serde_json::from_str(r#"["cat","dog"]"#).unwrap();
        let text: Vec<String> = strings.into_iter().map(Suggestion::into_text).collect();
        assert_eq!(text, vec!["cat", "dog"]);

        let objects: Vec<Suggestion> =
            serde_json::from_str(r#"[{"query":"cat hug"},{"name":"dog"}]"#).unwrap();
        let text: Vec<String> = objects.into_iter().map(Suggestion::into_text).collect();
        assert_eq!(text, vec!["cat hug", "dog"]);
    }

    proptest::proptest! {
        /// Somebody else's service, proxied. Any of these fields can be a
        /// number where a string was documented.
        #[test]
        fn a_mangled_result_is_refused_rather_than_a_panic(
            body in r#"\{("(id|title|url|width|preview)":(null|1|"x"|\[\]|\{\})){0,4}\}"#,
        ) {
            let _ = serde_json::from_str::<GifResult>(&body);
            let _ = serde_json::from_str::<Trending>(&body);
        }
    }
}
