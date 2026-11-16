/// Limits include the container and separators supplied by the protocol encoder.
#[derive(Clone, Copy)]
pub(crate) struct BatchLimits {
    pub max_items: usize,
    pub max_bytes: usize,
    pub container_bytes: usize,
    pub separator_bytes: usize,
}

/// Ordered groups: oversized items remain in their original position.
pub(crate) enum Partition<T> {
    Batch(Vec<T>),
    Oversized(T),
}

pub(crate) struct Batcher;

impl Batcher {
    pub fn partition<T>(
        items: Vec<T>,
        limits: BatchLimits,
        weight: impl Fn(&T) -> usize,
    ) -> Vec<Partition<T>> {
        let mut output = Vec::new();
        let mut batch = Vec::new();
        let mut bytes = limits.container_bytes;
        for item in items {
            let size = weight(&item);
            let singleton = limits.container_bytes.checked_add(size);
            let next = bytes
                .checked_add(if batch.is_empty() {
                    0
                } else {
                    limits.separator_bytes
                })
                .and_then(|bytes| bytes.checked_add(size));
            let fits = next.is_some_and(|bytes| bytes <= limits.max_bytes)
                && batch.len() < limits.max_items.max(1);
            if !batch.is_empty() && !fits {
                output.push(Partition::Batch(std::mem::take(&mut batch)));
                bytes = limits.container_bytes;
            }
            if singleton.is_none_or(|bytes| bytes > limits.max_bytes) {
                output.push(Partition::Oversized(item));
            } else {
                bytes += if batch.is_empty() {
                    0
                } else {
                    limits.separator_bytes
                };
                bytes += size;
                batch.push(item);
            }
        }
        if !batch.is_empty() {
            output.push(Partition::Batch(batch));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_in_order_with_explicit_oversized_items() {
        let parts = Batcher::partition(
            vec![2, 3, 50, 1, 1, 1],
            BatchLimits {
                max_items: 2,
                max_bytes: 8,
                container_bytes: 2,
                separator_bytes: 1,
            },
            |size| *size,
        );
        let parts: Vec<_> = parts
            .into_iter()
            .map(|part| match part {
                Partition::Batch(items) => (false, items),
                Partition::Oversized(item) => (true, vec![item]),
            })
            .collect();
        assert_eq!(
            parts,
            vec![
                (false, vec![2, 3]),
                (true, vec![50]),
                (false, vec![1, 1]),
                (false, vec![1])
            ]
        );
    }

    #[test]
    fn weights_cannot_wrap_and_empty_input_has_no_batches() {
        let limits = BatchLimits {
            max_items: 0,
            max_bytes: usize::MAX,
            container_bytes: 2,
            separator_bytes: 1,
        };
        assert!(Batcher::partition(Vec::<usize>::new(), limits, |size| *size).is_empty());
        assert!(matches!(
            Batcher::partition(vec![usize::MAX], limits, |size| *size)[0],
            Partition::Oversized(_)
        ));
    }
}
