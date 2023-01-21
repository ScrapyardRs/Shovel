use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pin_project_lite::pin_project;

pub trait AwaitingEntity {
    fn poll_tick(&mut self, cx: &mut Context) -> Result<bool, ()>;
}

impl<T> AwaitingEntity for &mut T
where
    T: AwaitingEntity,
{
    fn poll_tick(&mut self, cx: &mut Context) -> Result<bool, ()> {
        T::poll_tick(self, cx)
    }
}

pub trait CaptureAwaitingEntity {
    type AwaitingEntityOutput<'a>
    where
        Self: 'a;

    #[allow(clippy::needless_lifetimes)]
    fn capture<'a>(&'a mut self) -> Self::AwaitingEntityOutput<'a>;
}

pin_project! {
    pub struct AwaitingEntities<T> {
        pub(crate) entities: Vec<T>,
    }
}

impl<T> Future for AwaitingEntities<T>
where
    T: AwaitingEntity,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let me = self.project();
        let mut found_packets = false;
        for entity in me.entities.iter_mut() {
            match entity.poll_tick(cx) {
                Ok(true) => found_packets = true,
                Ok(false) | Err(()) => {}
            }
        }
        if found_packets {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[allow(clippy::needless_lifetimes)]
pub fn tick_entities<'a, T>(
    entities: &'a mut Vec<T>,
) -> AwaitingEntities<T::AwaitingEntityOutput<'a>>
where
    T: CaptureAwaitingEntity,
{
    AwaitingEntities {
        entities: entities.iter_mut().map(|entity| entity.capture()).collect(),
    }
}

pin_project! {
    pub struct EntityFactoryTick<T, E> {
        base: E,
        entities: AwaitingEntities<T>,
    }
}

impl<T, E> Future for EntityFactoryTick<T, E>
where
    T: AwaitingEntity,
    E: AwaitingEntity,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut me = self.project();
        let mut found_packets = false;

        match me.base.poll_tick(cx) {
            Ok(true) => found_packets = true,
            Ok(false) | Err(()) => {}
        }

        if let Poll::Ready(()) = Pin::new(&mut me.entities).poll(cx) {
            found_packets = true;
        };

        if found_packets {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub fn tick_factory<'a, T, E>(
    base: &'a mut E,
    entities: &'a mut Vec<T>,
) -> EntityFactoryTick<T::AwaitingEntityOutput<'a>, E::AwaitingEntityOutput<'a>>
where
    T: CaptureAwaitingEntity,
    E: CaptureAwaitingEntity,
{
    EntityFactoryTick {
        base: base.capture(),
        entities: tick_entities(entities),
    }
}

pub trait EntityFactory {
    type Base: CaptureAwaitingEntity;
    type Entity: CaptureAwaitingEntity;

    #[allow(clippy::needless_lifetimes)]
    fn split_factory_mut<'a>(&'a mut self) -> (&'a mut Self::Base, &'a mut Vec<Self::Entity>);

    #[allow(clippy::needless_lifetimes)]
    fn tick<'a>(
        &'a mut self,
    ) -> EntityFactoryTick<
        <Self::Entity as CaptureAwaitingEntity>::AwaitingEntityOutput<'a>,
        <Self::Base as CaptureAwaitingEntity>::AwaitingEntityOutput<'a>,
    > {
        let (base, entities) = self.split_factory_mut();
        tick_factory(base, entities)
    }
}
