use crate::Queue;

#[derive(thiserror::Error, Debug)]
pub enum Error {}

pub struct LocalQueue<I> {
    queue: Vec<I>,
}

impl<I> LocalQueue<I> {
    pub fn new() -> Self {
        let queue = Vec::new();
        Self { queue }
    }
}

impl<I> Queue for LocalQueue<I> {
    type Err = Error;
    type Item = I;

    fn enqueue(&mut self, item: Self::Item) -> Result<(), Self::Err> {
        let res = self.queue.push(item);
        Ok(res)
    }

    fn dequeue(&mut self) -> Option<Self::Item> {
        if !self.queue.is_empty() {
            Some(self.queue.remove(0))
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        self.queue.len()
    }
}

impl<I> Default for LocalQueue<I> {
    fn default() -> Self {
        Self::new()
    }
}

