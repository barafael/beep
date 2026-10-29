//! A connection's bytes one way, whole, with when each part of them came; or a stream's that
//! another protocol carries (HTTP/2's DATA frames).

use testbox_tools::tap::{Direction, Record};

/// The bytes one side sent, in order, and when the tap read each run of them.
#[derive(Default)]
pub(crate) struct Stream {
    pub(crate) bytes: Vec<u8>,
    /// Where each read began in `bytes`, and when it came: ascending.
    marks: Vec<(usize, u64)>,
    /// When that side closed, if it did.
    pub(crate) closed: Option<u64>,
}

impl Stream {
    /// The two sides of a connection, from its records: what the client sent, and what the
    /// server did.
    pub(crate) fn sides(records: &[Record]) -> (Self, Self) {
        let (mut sent, mut received) = (Self::default(), Self::default());
        for record in records {
            match record.direction {
                Direction::Sent => sent.push(record.at, &record.bytes),
                Direction::Received => received.push(record.at, &record.bytes),
                Direction::ClientClosed => sent.closed = Some(record.at),
                Direction::ServerClosed => received.closed = Some(record.at),
            }
        }
        (sent, received)
    }

    pub(crate) fn push(&mut self, at: u64, bytes: &[u8]) {
        self.marks.push((self.bytes.len(), at));
        self.bytes.extend_from_slice(bytes);
    }

    /// When the byte at `offset` came: the read it was in.
    pub(crate) fn at(&self, offset: usize) -> u64 {
        let index = self.marks.partition_point(|&(start, _)| start <= offset);
        index
            .checked_sub(1)
            .and_then(|index| self.marks.get(index))
            .map_or(0, |&(_, at)| at)
    }
}

#[cfg(test)]
mod tests {
    use super::Stream;
    use testbox_tools::tap::{Direction, Record};

    #[test]
    fn a_byte_came_with_its_read() {
        let record = |at, direction, bytes: &[u8]| Record {
            at,
            direction,
            bytes: bytes.to_vec(),
        };
        let (sent, received) = Stream::sides(&[
            record(10, Direction::Sent, b"abc"),
            record(20, Direction::Received, b"xy"),
            record(30, Direction::Sent, b"de"),
            record(40, Direction::ClientClosed, b""),
        ]);
        assert_eq!(sent.bytes, b"abcde");
        assert_eq!(
            (sent.at(0), sent.at(2), sent.at(3), sent.at(4)),
            (10, 10, 30, 30)
        );
        assert_eq!(sent.closed, Some(40));
        assert_eq!(
            (received.bytes.as_slice(), received.at(1)),
            (&b"xy"[..], 20)
        );
        assert_eq!(received.closed, None);
    }
}
