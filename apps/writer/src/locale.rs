use std::cell::Cell;

thread_local! { static ENGLISH: Cell<bool> = const { Cell::new(false) }; }

pub(crate) fn set_english(value: bool) {
    ENGLISH.set(value);
}

pub(crate) fn text(label: &str) -> &str {
    if !ENGLISH.get() {
        return label;
    }
    ENGLISH_LABELS
        .lines()
        .find_map(|line| {
            let (zh, en) = line.split_once('\t')?;
            (zh == label).then_some(en)
        })
        .unwrap_or(label)
}

const ENGLISH_LABELS: &str = include_str!("locale/en.tsv");
