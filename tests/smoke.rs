use std::io::BufReader;

use mail_parser::{mailbox::mbox::MessageIterator, MessageParser};

#[test]
fn sample_mbox_has_two_messages_and_one_attachment() {
    let file = std::fs::File::open("tests/fixtures/sample.mbox").unwrap();
    let mut count = 0usize;
    let mut second_attachment_count = None;

    for raw in MessageIterator::new(BufReader::new(file)) {
        let raw = raw.unwrap();
        let message = MessageParser::default().parse(raw.contents()).unwrap();
        count += 1;
        if count == 2 {
            second_attachment_count = Some(message.attachment_count());
        }
    }

    assert_eq!(count, 2);
    assert_eq!(second_attachment_count, Some(1));
}
