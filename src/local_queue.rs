use crate::Queue;
use indexmap::IndexMap;
use std::hash::Hash;

#[derive(thiserror::Error, Debug)]
pub enum Error {}

#[derive(Debug)]
pub struct LocalQueue<Id, Item> {
    queue: IndexMap<Id, Item>,
    watchers: Vec<async_std::channel::Sender<Item>>,
}

impl<Id, Item> LocalQueue<Id, Item> {
    pub fn new() -> Self {
        let queue = IndexMap::new();
        let watchers = vec![];
        Self { queue, watchers }
    }

    pub fn register_watcher(&mut self, sender: async_std::channel::Sender<Item>) {
        self.watchers.push(sender);
    }
}

impl<Id, Item> Queue for LocalQueue<Id, Item>
where
    Id: Hash + Eq,
    Item: Send + 'static,
{
    type Err = Error;
    type Id = Id;
    type Item = Item;

    fn add(&mut self, id: Self::Id, item: Self::Item) {
        if !self.watchers.is_empty() {
            let watcher = self.watchers.remove(0);
            async_std::task::spawn(async move { watcher.send(item).await });
        } else {
            self.queue.insert_full(id, item);
        }
    }

    fn remove(&mut self) -> Option<Self::Item> {
        if !self.queue.is_empty() {
            self.queue.shift_remove_index(0).map(|(_k, v)| v)
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn pos(&self, id: Self::Id) -> Option<usize> {
        self.queue.get_index_of(&id)
    }
}

impl<Id, Item> Default for LocalQueue<Id, Item> {
    fn default() -> Self {
        Self::new()
    }
}
