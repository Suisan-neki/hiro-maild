use std::io::BufReader;

use mail_parser::{mailbox::mbox::MessageIterator, MessageParser};

#[test]
fn sample_mbox_has_two_messages() {
    let file = std::fs::File::open("tests/fixtures/sample.mbox").unwrap();
    let messages = MessageIterator::new(BufReader::new(file))
        .map(|raw| {
            let raw = raw.unwrap();
            MessageParser::default().parse(raw.contents()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].attachment_count(), 1);
}
