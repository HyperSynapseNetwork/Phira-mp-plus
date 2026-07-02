use fluent::concurrent::FluentBundle;
use fluent::{FluentArgs, FluentResource};
use once_cell::sync::Lazy;
use std::sync::Arc;
use tokio::task_local;
use tracing::error;
use unic_langid::LanguageIdentifier;

pub static LOCALE: Lazy<LanguageIdentifier> = Lazy::new(|| "en-US".parse().unwrap());

#[derive(Debug, Clone)]
pub struct Language(pub LanguageIdentifier);

impl Default for Language {
    fn default() -> Self {
        Self(LOCALE.clone())
    }
}

impl std::str::FromStr for Language {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let s = s.replace('_', "-");
        let id: LanguageIdentifier = s.parse().map_err(|_| ())?;
        Ok(Self(id))
    }
}

type Bundle = FluentBundle<FluentResource>;

fn make_bundle(source: &str) -> Bundle {
    let res = FluentResource::try_new(source.to_owned()).expect("failed to create resource");
    let langid: LanguageIdentifier = "en-US".parse().expect("valid langid");
    let mut bundle = FluentBundle::new_concurrent(vec![langid]);
    bundle.add_resource(res).expect("failed to add resource");
    bundle
}

// Concurrent FluentBundle is Send+Sync, so we can use Lazy directly
pub static EN_BUNDLE: Lazy<Bundle> =
    Lazy::new(|| make_bundle(include_str!("../locales/en-US.ftl")));
pub static ZH_CN_BUNDLE: Lazy<Bundle> =
    Lazy::new(|| make_bundle(include_str!("../locales/zh-CN.ftl")));
pub static ZH_TW_BUNDLE: Lazy<Bundle> =
    Lazy::new(|| make_bundle(include_str!("../locales/zh-TW.ftl")));

task_local! {
    pub static LANGUAGE: Arc<Language>;
}

#[macro_export]
macro_rules! tl {
    ($id:expr) => {{
        let lang = $crate::l10n::LANGUAGE.get();
        let id: &str = $id;
        $crate::l10n::try_translate(&lang.0, id)
    }};
    ($id:expr, $($key:ident => $value:expr),* $(,)?) => {{
        let lang = $crate::l10n::LANGUAGE.get();
        let id: &str = $id;
        let mut args = fluent::FluentArgs::new();
        $(
            args.set(stringify!($key), $value);
        )*
        $crate::l10n::try_translate_with_args(&lang.0, id, args)
    }};
}

pub fn try_translate(lang: &LanguageIdentifier, id: &str) -> String {
    match lang.to_string().as_str() {
        "zh-CN" => translate_bundle(&ZH_CN_BUNDLE, id),
        "zh-TW" => translate_bundle(&ZH_TW_BUNDLE, id),
        _ => translate_bundle(&EN_BUNDLE, id),
    }
}

pub fn try_translate_with_args(lang: &LanguageIdentifier, id: &str, args: FluentArgs) -> String {
    match lang.to_string().as_str() {
        "zh-CN" => translate_bundle_with_args(&ZH_CN_BUNDLE, id, args),
        "zh-TW" => translate_bundle_with_args(&ZH_TW_BUNDLE, id, args),
        _ => translate_bundle_with_args(&EN_BUNDLE, id, args),
    }
}

fn translate_bundle(bundle: &Bundle, id: &str) -> String {
    let Some(msg) = bundle.get_message(id) else {
        error!("failed to get message {id:?}");
        return id.to_owned();
    };
    let Some(value) = msg.value() else {
        error!("failed to get value of {id:?}");
        return id.to_owned();
    };
    let mut errors = vec![];
    let translated = bundle.format_pattern(value, None, &mut errors);
    if !errors.is_empty() {
        error!("failed to format message {id:?}: {errors:?}");
    }
    translated.into_owned()
}

fn translate_bundle_with_args(bundle: &Bundle, id: &str, args: FluentArgs) -> String {
    let Some(msg) = bundle.get_message(id) else {
        error!("failed to get message {id:?}");
        return id.to_owned();
    };
    let Some(value) = msg.value() else {
        error!("failed to get value of {id:?}");
        return id.to_owned();
    };
    let mut errors = vec![];
    let translated = bundle.format_pattern(value, Some(&args), &mut errors);
    if !errors.is_empty() {
        error!("failed to format message {id:?}: {errors:?}");
    }
    translated.into_owned()
}
