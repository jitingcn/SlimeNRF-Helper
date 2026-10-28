use slimenrf_terminal_core::TerminalBuffer;
use std::collections::HashMap;
use std::rc::Rc;

#[test]
fn incremental_row_consumer_updates_newline_without_recopying_unchanged_rows() {
    fn sync_rows(
        cache: &mut HashMap<(u64, u64), Rc<str>>,
        terminal: &TerminalBuffer,
    ) -> Vec<Rc<str>> {
        let mut previous = std::mem::take(cache);
        terminal
            .rows()
            .map(|(id, revision, text, _, newline)| {
                let key = (id, revision);
                let row = previous.remove(&key).unwrap_or_else(|| {
                    let mut row = String::with_capacity(text.len() + if newline { 1 } else { 0 });
                    row.push_str(text);
                    if newline {
                        row.push('\n');
                    }
                    Rc::from(row)
                });
                cache.insert(key, Rc::clone(&row));
                row
            })
            .collect()
    }

    let mut terminal = TerminalBuffer::new();
    terminal.push("stable\n界".as_bytes());
    let mut cache = HashMap::new();
    let first = sync_rows(&mut cache, &terminal);
    assert_eq!(
        first.iter().map(|row| row.as_ref()).collect::<String>(),
        "stable\n界"
    );

    terminal.push(b"\n");
    let next = sync_rows(&mut cache, &terminal);
    assert_eq!(
        next.iter().map(|row| row.as_ref()).collect::<String>(),
        "stable\n界\n"
    );
    assert!(Rc::ptr_eq(&first[0], &next[0]));
    assert!(!Rc::ptr_eq(&first[1], &next[1]));
    assert_eq!(next.len(), 3);
    assert!(next[2].is_empty());
}
