mod local_queue;
pub use local_queue::LocalQueue;

pub trait Queue {
    type Err;
    type Item;

    fn enqueue(&mut self, item: Self::Item) -> Result<(), Self::Err>;
    fn dequeue(&mut self) -> Option<Self::Item>;
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
