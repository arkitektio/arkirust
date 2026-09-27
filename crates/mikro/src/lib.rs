//! Mikro for Rust: image data in Arkitekt.
//!
//! Add the service to an app and take the client in an action:
//!
//! ```ignore
//! use arkitekt::{action, App};
//! use mikro::{ArrayDataset, Mikro};
//!
//! /// Create an image
//! #[action]
//! async fn create_image(name: String, #[inject] mikro: Mikro) -> anyhow::Result<ArrayDataset> {
//!     let data = ndarray::ArrayD::<u16>::zeros(vec![512, 512]);
//!     Ok(mikro.create_array_dataset(&name, &data, mikro::axes_for(&["y", "x"])).await?)
//! }
//!
//! App::new("my-app", "0.1.0").service(mikro::service).action(create_image).run().await
//! ```
//!
//! An [`ArrayDataset`] is a [`Structure`](arkitekt::Structure): it travels as
//! `@mikro/arraydataset` and is expanded back through the [`Mikro`] client.

pub mod api;
mod array;
mod datalayer;
mod models;
mod specs;

use arkitekt::rath::Rath;
use ndarray::ArrayD;

pub use crate::api::create_array_dataset::{AxisInput, AxisType};
pub use crate::array::{chunk_shape, MikroElement};
pub use crate::datalayer::{DataLayer, Grant, GrantStore};
pub use crate::models::{axes_for, ArrayDataset, DataArray, ZarrStore, SEARCH_ARRAY_DATASETS};
pub use crate::specs::service;

#[derive(Debug, thiserror::Error)]
pub enum MikroError {
    #[error(transparent)]
    GraphQL(#[from] arkitekt::rath::RathError),
    #[error("storage: {0}")]
    Storage(#[from] object_store::Error),
    #[error("zarr: {0}")]
    Zarr(String),
    #[error("shape: {0}")]
    Shape(String),
    #[error("unexpected response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("{0}")]
    NotFound(String),
}

macro_rules! zarr_error {
    ($($ty:ty),+) => {$(
        impl From<$ty> for MikroError {
            fn from(e: $ty) -> Self {
                MikroError::Zarr(e.to_string())
            }
        }
    )+};
}

zarr_error!(
    zarrs::array::ArrayCreateError,
    zarrs::array::ArrayError,
    zarrs::storage::StorageError
);

pub type Result<T, E = MikroError> = std::result::Result<T, E>;

/// The mikro client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Mikro {
    rath: Rath,
    datalayer: DataLayer,
}

impl Mikro {
    pub fn new(rath: Rath, datalayer: DataLayer) -> Self {
        Self { rath, datalayer }
    }

    /// The GraphQL client, for operations this crate does not wrap.
    pub fn rath(&self) -> &Rath {
        &self.rath
    }

    pub fn datalayer(&self) -> &DataLayer {
        &self.datalayer
    }

    /// Upload an array as zarr and return its store id (what an `ArrayLike` input takes).
    ///
    /// Three steps, like the Python client: request a grant, write the array
    /// under the granted prefix, then tell mikro the upload is complete.
    pub async fn upload_array<T: MikroElement>(
        &self,
        array: &ArrayD<T>,
        dimension_names: &[String],
    ) -> Result<String> {
        use api::request_zarr_upload::{RequestZarrUploadInput, Variables};

        // An empty input, exactly as the Python client sends it.
        let grant = self
            .rath
            .execute::<api::RequestZarrUpload>(Variables {
                input: RequestZarrUploadInput {
                    shape: None,
                    chunks: None,
                    version: None,
                    host: None,
                    port: None,
                },
            })
            .await?
            .request_zarr_upload;

        let store = self.datalayer.store(&Grant {
            access_key: grant.access_key,
            secret_key: grant.secret_key,
            session_token: grant.session_token,
            bucket: grant.bucket,
            key: grant.key,
        })?;
        let written = array::write_array(store, array, dimension_names).await;

        let finished = self
            .rath
            .execute::<api::FinishZarrUpload>(api::finish_zarr_upload::Variables {
                input: api::finish_zarr_upload::FinishZarrUploadInput {
                    store_id: grant.store.clone(),
                    valid: written.is_ok(),
                },
            })
            .await;
        written?;
        finished?;
        Ok(grant.store)
    }

    /// Upload `data` and create an array dataset from it.
    ///
    /// `axes` names and types each dimension, in order (see [`axes_for`]).
    pub async fn create_array_dataset<T: MikroElement>(
        &self,
        name: &str,
        data: &ArrayD<T>,
        axes: Vec<AxisInput>,
    ) -> Result<ArrayDataset> {
        self.create_array_dataset_in(name, data, axes, None).await
    }

    /// Like [`create_array_dataset`](Self::create_array_dataset), inside a folder.
    pub async fn create_array_dataset_in<T: MikroElement>(
        &self,
        name: &str,
        data: &ArrayD<T>,
        axes: Vec<AxisInput>,
        folder: Option<String>,
    ) -> Result<ArrayDataset> {
        use api::create_array_dataset::{CreateArrayDatasetInput, Variables};

        if axes.len() != data.ndim() {
            return Err(MikroError::Shape(format!(
                "{} axes for a {}-dimensional array",
                axes.len(),
                data.ndim()
            )));
        }
        let names: Vec<String> = axes.iter().map(|a| a.name.clone()).collect();
        let store = self.upload_array(data, &names).await?;

        let created = self
            .rath
            .execute::<api::CreateArrayDataset>(Variables {
                input: CreateArrayDatasetInput {
                    // `data` is level 0; `scales` lists the coarser levels 1..N only.
                    data: store,
                    scales: vec![],
                    name: name.to_owned(),
                    axes,
                    folder,
                    anchors: None,
                    derived_from: None,
                    source_files: None,
                },
            })
            .await?;
        ArrayDataset::from_fragment(&created.create_array_dataset)
    }

    pub async fn get_array_dataset(&self, id: &str) -> Result<ArrayDataset> {
        let data = self
            .rath
            .execute::<api::GetArrayDataset>(api::get_array_dataset::Variables {
                id: id.to_owned(),
            })
            .await?;
        ArrayDataset::from_fragment(&data.array_dataset)
    }

    /// Download the data of one scale level (0 is full resolution).
    pub async fn read_array<T: MikroElement>(
        &self,
        dataset: &ArrayDataset,
        level: i64,
    ) -> Result<ArrayD<T>> {
        let data_array = dataset
            .data_arrays
            .iter()
            .find(|a| a.level == level)
            .ok_or_else(|| MikroError::NotFound(format!("{} has no level {level}", dataset.id)))?;
        let grant = self
            .rath
            .execute::<api::RequestZarrAccess>(api::request_zarr_access::Variables {
                input: api::request_zarr_access::RequestZarrAccessInput {
                    store_id: data_array.store.id.clone(),
                },
            })
            .await?
            .request_zarr_access;
        let store = self.datalayer.store(&Grant {
            access_key: grant.access_key,
            secret_key: grant.secret_key,
            session_token: grant.session_token,
            bucket: grant.bucket,
            key: grant.key,
        })?;
        array::read_array(store).await
    }
}
