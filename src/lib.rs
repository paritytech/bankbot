mod job;
mod local_queue;

pub use job::Job;
pub use local_queue::LocalQueue;

pub trait Queue {
    type Err;
    type Id;
    type Item;

    fn add(&mut self, id: Self::Id, item: Self::Item) -> usize;
    fn remove(&mut self) -> Option<Self::Item>;
    fn len(&self) -> usize;
    fn pos(&self, id: Self::Id) -> Option<usize>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
