//! Flat forward drawing pass that mimics a blit.

use derivative::Derivative;
use gfx::pso::buffer::ElemStride;
use gfx_core::state::{Blend, ColorMask};
use glsl_layout::Uniform;
use log::warn;
use std::marker::PhantomData;

use amethyst_assets::{AssetStorage, Handle};
use amethyst_core::{
    nalgebra::{alga::general::SubsetOf, convert, one, zero, Real, Vector4},
    specs::prelude::{Join, Read, ReadStorage},
    transform::Transform,
};
use amethyst_error::Error;

use crate::{
    cam::{ActiveCamera, Camera},
    hidden::{Hidden, HiddenPropagate},
    mesh::MeshHandle,
    pass::util::{
        add_texture, default_transparency, get_camera, set_view_args, setup_textures, ViewArgs,
    },
    pipe::{
        pass::{Pass, PassData},
        DepthMode, Effect, NewEffect,
    },
    sprite::{Flipped, SpriteRender, SpriteSheet},
    sprite_visibility::SpriteVisibility,
    tex::{Texture, TextureHandle},
    types::{Encoder, Factory, Slice},
    vertex::{Attributes, Query, VertexFormat},
    Color, Rgba,
};

use super::*;

/// Draws sprites on a 2D quad.
///
/// # Type Parameters:
///
/// * `N`: `RealBound` (f32, f64)
#[derive(Derivative, Clone, Debug)]
#[derivative(Default(bound = "Self: Pass"))]
pub struct DrawFlat2D<N>
where
    N: Real,
{
    #[derivative(Default(value = "default_transparency()"))]
    transparency: Option<(ColorMask, Blend, Option<DepthMode>)>,
    batch: TextureBatch<N>,
    _pd: PhantomData<N>,
}

impl<N> DrawFlat2D<N>
where
    Self: Pass,
    N: Real,
{
    /// Create instance of `DrawFlat2D` pass
    pub fn new() -> Self {
        Default::default()
    }

    /// Transparency is enabled by default.
    /// If you pass false to this function transparency will be disabled.
    ///
    /// If you pass true and this was disabled previously default settings will be reinstated.
    /// If you pass true and this was already enabled this will do nothing.
    pub fn with_transparency(mut self, input: bool) -> Self {
        if input {
            if self.transparency.is_none() {
                self.transparency = default_transparency();
            }
        } else {
            self.transparency = None;
        }
        self
    }

    /// Set transparency settings to custom values.
    pub fn with_transparency_settings(
        mut self,
        mask: ColorMask,
        blend: Blend,
        depth: Option<DepthMode>,
    ) -> Self {
        self.transparency = Some((mask, blend, depth));
        self
    }

    fn attributes() -> Attributes<'static> {
        <SpriteInstance as Query<(DirX, DirY, Pos, OffsetU, OffsetV, Depth, Color)>>::QUERIED_ATTRIBUTES
    }
}

impl<'a, N: Real> PassData<'a> for DrawFlat2D<N> {
    type Data = (
        Read<'a, ActiveCamera>,
        ReadStorage<'a, Camera>,
        Read<'a, AssetStorage<SpriteSheet>>,
        Read<'a, AssetStorage<Texture>>,
        Option<Read<'a, SpriteVisibility>>,
        ReadStorage<'a, Hidden>,
        ReadStorage<'a, HiddenPropagate>,
        ReadStorage<'a, SpriteRender>,
        ReadStorage<'a, Transform<N>>,
        ReadStorage<'a, TextureHandle>,
        ReadStorage<'a, Flipped>,
        ReadStorage<'a, MeshHandle>,
        ReadStorage<'a, Rgba>,
    );
}

impl<N: Real> Pass for DrawFlat2D<N> {
    fn compile(&mut self, effect: NewEffect<'_>) -> Result<Effect, Error> {
        use std::mem;

        let mut builder = effect.simple(VERT_SRC, FRAG_SRC);
        builder
            .without_back_face_culling()
            .with_raw_constant_buffer(
                "ViewArgs",
                mem::size_of::<<ViewArgs as Uniform>::Std140>(),
                1,
            )
            .with_raw_vertex_buffer(Self::attributes(), SpriteInstance::size() as ElemStride, 1);
        setup_textures(&mut builder, &TEXTURES);
        match self.transparency {
            Some((mask, blend, depth)) => builder.with_blended_output("color", mask, blend, depth),
            None => builder.with_output("color", Some(DepthMode::LessEqualWrite)),
        };
        builder.build()
    }

    fn apply<'a, 'b: 'a>(
        &'a mut self,
        encoder: &mut Encoder,
        effect: &mut Effect,
        mut factory: Factory,
        (
            active,
            camera,
            sprite_sheet_storage,
            tex_storage,
            visibility,
            hidden,
            hidden_prop,
            sprite_render,
            transform,
            texture_handle,
            flipped,
            mesh,
            rgba,
        ): <Self as PassData<'a>>::Data,
    ) {
        let camera = get_camera(active, &camera, &transform);

        match visibility {
            None => {
                for (sprite_render, transform, flipped, rgba, _, _) in (
                    &sprite_render,
                    &transform,
                    flipped.maybe(),
                    rgba.maybe(),
                    !&hidden,
                    !&hidden_prop,
                )
                    .join()
                {
                    self.batch.add_sprite(
                        sprite_render,
                        Some(transform),
                        flipped,
                        rgba,
                        &sprite_sheet_storage,
                        &tex_storage,
                    );
                }

                for (image_render, transform, flipped, rgba, _, _, _) in (
                    &texture_handle,
                    &transform,
                    flipped.maybe(),
                    rgba.maybe(),
                    !&hidden,
                    !&hidden_prop,
                    !&mesh,
                )
                    .join()
                {
                    self.batch.add_image(
                        image_render,
                        Some(transform),
                        flipped,
                        rgba,
                        &tex_storage,
                    );
                }

                self.batch.sort();
            }
            Some(ref visibility) => {
                for (sprite_render, transform, flipped, rgba, _) in (
                    &sprite_render,
                    &transform,
                    flipped.maybe(),
                    rgba.maybe(),
                    &visibility.visible_unordered,
                )
                    .join()
                {
                    self.batch.add_sprite(
                        sprite_render,
                        Some(transform),
                        flipped,
                        rgba,
                        &sprite_sheet_storage,
                        &tex_storage,
                    );
                }

                for (image_render, transform, flipped, rgba, _, _) in (
                    &texture_handle,
                    &transform,
                    flipped.maybe(),
                    rgba.maybe(),
                    &visibility.visible_unordered,
                    !&mesh,
                )
                    .join()
                {
                    self.batch.add_image(
                        image_render,
                        Some(transform),
                        flipped,
                        rgba,
                        &tex_storage,
                    );
                }

                // We are free to optimize the order of the opaque sprites.
                self.batch.sort();

                for entity in &visibility.visible_ordered {
                    if let Some(sprite_render) = sprite_render.get(*entity) {
                        self.batch.add_sprite(
                            sprite_render,
                            transform.get(*entity),
                            flipped.get(*entity),
                            rgba.get(*entity),
                            &sprite_sheet_storage,
                            &tex_storage,
                        );
                    } else if let Some(texture_handle) = texture_handle.get(*entity) {
                        self.batch.add_image(
                            texture_handle,
                            transform.get(*entity),
                            flipped.get(*entity),
                            rgba.get(*entity),
                            &tex_storage,
                        )
                    }
                }
            }
        }
        self.batch.encode(
            encoder,
            &mut factory,
            effect,
            camera,
            &sprite_sheet_storage,
            &tex_storage,
        );
        self.batch.reset();
    }
}

#[derive(Clone, Debug)]
enum TextureDrawData<N: Real> {
    Sprite {
        texture_handle: Handle<Texture>,
        render: SpriteRender,
        flipped: Option<Flipped>,
        rgba: Option<Rgba>,
        transform: Transform<N>,
    },
    Image {
        texture_handle: Handle<Texture>,
        transform: Transform<N>,
        flipped: Option<Flipped>,
        rgba: Option<Rgba>,
        width: usize,
        height: usize,
    },
}

impl<N: Real> TextureDrawData<N> {
    pub fn texture_handle(&self) -> &Handle<Texture> {
        match self {
            TextureDrawData::Sprite { texture_handle, .. } => texture_handle,
            TextureDrawData::Image { texture_handle, .. } => texture_handle,
        }
    }

    pub fn tex_id(&self) -> u32 {
        match self {
            TextureDrawData::Sprite { texture_handle, .. } => texture_handle.id(),
            TextureDrawData::Image { texture_handle, .. } => texture_handle.id(),
        }
    }

    pub fn flipped(&self) -> &Option<Flipped> {
        match self {
            TextureDrawData::Sprite { flipped, .. } => flipped,
            TextureDrawData::Image { flipped, .. } => flipped,
        }
    }
}

#[derive(Clone, Default, Debug)]
struct TextureBatch<N: Real> {
    textures: Vec<TextureDrawData<N>>,
}

impl<N: Real> TextureBatch<N> {
    pub fn add_image(
        &mut self,
        texture_handle: &TextureHandle,
        transform: Option<&Transform<N>>,
        flipped: Option<&Flipped>,
        rgba: Option<&Rgba>,
        tex_storage: &AssetStorage<Texture>,
    ) {
        let transform = match transform {
            Some(v) => v,
            None => return,
        };

        let texture_dims = match tex_storage.get(&texture_handle) {
            Some(tex) => tex.size(),
            None => {
                warn!("Texture not loaded for texture: `{:?}`.", texture_handle);
                return;
            }
        };

        self.textures.push(TextureDrawData::Image {
            texture_handle: texture_handle.clone(),
            transform: *transform,
            flipped: flipped.cloned(),
            rgba: rgba.cloned(),
            width: texture_dims.0,
            height: texture_dims.1,
        });
    }

    pub fn add_sprite(
        &mut self,
        sprite_render: &SpriteRender,
        transform: Option<&Transform<N>>,
        flipped: Option<&Flipped>,
        rgba: Option<&Rgba>,
        sprite_sheet_storage: &AssetStorage<SpriteSheet>,
        tex_storage: &AssetStorage<Texture>,
    ) {
        let transform = match transform {
            Some(v) => v,
            None => return,
        };

        let texture_handle = match sprite_sheet_storage.get(&sprite_render.sprite_sheet) {
            Some(sprite_sheet) => {
                if tex_storage.get(&sprite_sheet.texture).is_none() {
                    warn!(
                        "Texture not loaded for texture: `{:?}`.",
                        sprite_sheet.texture
                    );
                    return;
                }

                sprite_sheet.texture.clone()
            }
            None => {
                warn!(
                    "Sprite sheet not loaded for sprite_render: `{:?}`.",
                    sprite_render
                );
                return;
            }
        };

        self.textures.push(TextureDrawData::Sprite {
            texture_handle,
            render: sprite_render.clone(),
            flipped: flipped.cloned(),
            rgba: rgba.cloned(),
            transform: *transform,
        });
    }

    /// Optimize the sprite order to generating more coherent batches.
    pub fn sort(&mut self) {
        // Only takes the texture into account for now.
        self.textures.sort_by(|a, b| a.tex_id().cmp(&b.tex_id()));
    }

    pub fn encode(
        &self,
        encoder: &mut Encoder,
        factory: &mut Factory,
        effect: &mut Effect,
        camera: Option<(&Camera, &Transform<N>)>,
        sprite_sheet_storage: &AssetStorage<SpriteSheet>,
        tex_storage: &AssetStorage<Texture>,
    ) {
        use gfx::{
            buffer,
            memory::{Bind, Typed},
            Factory,
        };

        if self.textures.is_empty() {
            return;
        }

        // Sprite vertex shader
        set_view_args(effect, encoder, camera);

        // We might be able to improve performance here if we
        // preallocate the maximum needed capacity. We need to
        // iterate over the sprites though to find out the longest
        // chain of sprites with the same texture, so we would need
        // to check if it actually results in an improvement over just
        // doing the allocations.
        let mut instance_data = Vec::<f32>::new();
        let mut num_instances = 0;
        let num_quads = self.textures.len();

        for (i, quad) in self.textures.iter().enumerate() {
            let texture = tex_storage
                .get(&quad.texture_handle())
                .expect("Unable to get texture of sprite");

            let (flip_horizontal, flip_vertical) = match quad.flipped() {
                Some(Flipped::Horizontal) => (true, false),
                Some(Flipped::Vertical) => (false, true),
                Some(Flipped::Both) => (true, true),
                _ => (false, false),
            };

            let (dir_x, dir_y, pos, uv_left, uv_right, uv_top, uv_bottom, rgba) = match quad {
                TextureDrawData::Sprite {
                    render,
                    transform,
                    rgba,
                    ..
                } => {
                    let sprite_sheet = sprite_sheet_storage
                        .get(&render.sprite_sheet)
                        .expect(
                            "Unreachable: Existence of sprite sheet checked when collecting the sprites",
                        );

                    // Append sprite to instance data.
                    let sprite_data = &sprite_sheet.sprites[render.sprite_number];

                    let tex_coords = &sprite_data.tex_coords;
                    let (uv_left, uv_right) = if flip_horizontal {
                        (tex_coords.right, tex_coords.left)
                    } else {
                        (tex_coords.left, tex_coords.right)
                    };
                    let (uv_bottom, uv_top) = if flip_vertical {
                        (tex_coords.top, tex_coords.bottom)
                    } else {
                        (tex_coords.bottom, tex_coords.top)
                    };

                    let global_matrix = &transform.global_matrix();

                    let dir_x = global_matrix.column(0) * sprite_data.width;
                    let dir_y = global_matrix.column(1) * sprite_data.height;

                    // The offsets are negated to shift the sprite left and down relative to the entity, in
                    // regards to pivot points. This is the convention adopted in:
                    //
                    // * libgdx: <https://gamedev.stackexchange.com/q/22553>
                    // * godot: <https://godotengine.org/qa/9784>
                    let pos = global_matrix
                        * Vector4::new(-sprite_data.offsets[0], -sprite_data.offsets[1], 0.0, 1.0);

                    (
                        dir_x, dir_y, pos, uv_left, uv_right, uv_top, uv_bottom, rgba,
                    )
                }
                TextureDrawData::Image {
                    transform,
                    width,
                    height,
                    rgba,
                    ..
                } => {
                    let (uv_left, uv_right) = if flip_horizontal {
                        (1.0, 0.0)
                    } else {
                        (0.0, 1.0)
                    };
                    let (uv_bottom, uv_top) = if flip_vertical {
                        (1.0, 0.0)
                    } else {
                        (0.0, 1.0)
                    };

                    let global_matrix = &transform.global_matrix();

                    let dir_x = global_matrix.column(0) * (*width as f32);
                    let dir_y = global_matrix.column(1) * (*height as f32);

                    let pos = global_matrix * Vector4::<N>::new(one(), one(), zero(), one());

                    (
                        dir_x, dir_y, pos, uv_left, uv_right, uv_top, uv_bottom, rgba,
                    )
                }
            };
            let rgba = rgba.unwrap_or(Rgba::WHITE);
            instance_data.extend(&[
                dir_x.x, dir_x.y, dir_y.x, dir_y.y, pos.x, pos.y, uv_left, uv_right, uv_bottom,
                uv_top, pos.z, rgba.0, rgba.1, rgba.2, rgba.3,
            ]);
            num_instances += 1;

            // Need to flush outstanding draw calls due to state switch (texture).
            //
            // 1. We are at the last sprite and want to submit all pending work.
            // 2. The next sprite will use a different texture triggering a flush.
            let need_flush = i >= num_quads - 1
                || self.textures[i + 1].texture_handle().id() != quad.texture_handle().id();

            if need_flush {
                add_texture(effect, texture);

                let vbuf = factory
                    .create_buffer_immutable(&instance_data, buffer::Role::Vertex, Bind::empty())
                    .expect("Unable to create immutable buffer for `TextureBatch`");

                for _ in DrawFlat2D::attributes() {
                    effect.data.vertex_bufs.push(vbuf.raw().clone());
                }

                effect.draw(
                    &Slice {
                        start: 0,
                        end: 6,
                        base_vertex: 0,
                        instances: Some((num_instances, 0)),
                        buffer: Default::default(),
                    },
                    encoder,
                );

                effect.clear();

                num_instances = 0;
                instance_data.clear();
            }
        }
    }

    pub fn reset(&mut self) {
        self.textures.clear();
    }
}
