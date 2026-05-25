use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt, Transformation, percentage, pulsating_between, size,
};

use crate::{prelude::*, traits::transformable::Transformable};

/// An extension trait for adding common animations to animatable components.
pub trait CommonAnimationExt: AnimationExt {
    /// Render this component as rotating over the given duration.
    ///
    /// NOTE: This method uses the location of the caller to generate an ID for this state.
    ///       If this is not sufficient to identify your state (e.g. you're rendering a list item),
    ///       you can provide a custom ElementID using the `use_keyed_rotate_animation` method.
    #[track_caller]
    fn with_rotate_animation(self, duration: u64) -> AnimationElement<Self>
    where
        Self: Transformable + Sized,
    {
        self.with_keyed_rotate_animation(
            ElementId::CodeLocation(*std::panic::Location::caller()),
            duration,
        )
    }

    /// Render this component as rotating with the given element ID over the given duration.
    fn with_keyed_rotate_animation(
        self,
        id: impl Into<ElementId>,
        duration: u64,
    ) -> AnimationElement<Self>
    where
        Self: Transformable + Sized,
    {
        self.with_animation(
            id,
            Animation::new(Duration::from_secs(duration)).repeat(),
            |component, delta| component.transform(Transformation::rotate(percentage(delta))),
        )
    }

    /// Render this component as scale-pulsing ("breathing") between the given
    /// min and max scale factors over the given duration.
    ///
    /// NOTE: This method uses the location of the caller to generate an ID for this state.
    ///       If this is not sufficient to identify your state (e.g. you're rendering a list item),
    ///       use `with_keyed_scale_pulse_animation` to provide a custom ElementID.
    #[track_caller]
    fn with_scale_pulse_animation(
        self,
        duration: u64,
        min_scale: f32,
        max_scale: f32,
    ) -> AnimationElement<Self>
    where
        Self: Transformable + Sized,
    {
        self.with_keyed_scale_pulse_animation(
            ElementId::CodeLocation(*std::panic::Location::caller()),
            duration,
            min_scale,
            max_scale,
        )
    }

    /// Render this component as scale-pulsing with the given element ID over the given duration.
    fn with_keyed_scale_pulse_animation(
        self,
        id: impl Into<ElementId>,
        duration: u64,
        min_scale: f32,
        max_scale: f32,
    ) -> AnimationElement<Self>
    where
        Self: Transformable + Sized,
    {
        self.with_animation(
            id,
            Animation::new(Duration::from_secs(duration))
                .repeat()
                .with_easing(pulsating_between(min_scale, max_scale)),
            |component, delta| component.transform(Transformation::scale(size(delta, delta))),
        )
    }
}

impl<T: AnimationExt> CommonAnimationExt for T {}
