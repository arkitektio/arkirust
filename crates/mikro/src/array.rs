//! Writing and reading n-dimensional arrays as zarr v3.

use std::sync::Arc;

use ndarray::ArrayD;
use zarrs::array::codec::ZstdCodec;
use zarrs::array::{data_type, Array, ArrayBuilder, DataType, Element, ElementOwned, FillValue};

use crate::MikroError;
use zarrs::storage::{AsyncReadableStorageTraits, AsyncReadableWritableStorageTraits};

mod sealed {
    pub trait Sealed {}
}

/// Element types mikro can store.
pub trait MikroElement:
    Element + ElementOwned + Copy + Default + Into<FillValue> + Send + Sync + 'static + sealed::Sealed
{
    fn data_type() -> DataType;
}

macro_rules! element {
    ($($ty:ty => $dt:ident),+ $(,)?) => {$(
        impl sealed::Sealed for $ty {}
        impl MikroElement for $ty {
            fn data_type() -> DataType {
                data_type::$dt()
            }
        }
    )+};
}

element!(
    u8 => uint8, u16 => uint16, u32 => uint32, u64 => uint64,
    i8 => int8, i16 => int16, i32 => int32, i64 => int64,
    f32 => float32, f64 => float64,
);

/// Aim for chunks of about this many bytes (like the Python client).
const TARGET_CHUNK_BYTES: u64 = 20 * 1024 * 1024;

/// A chunk shape of roughly [`TARGET_CHUNK_BYTES`], filling the trailing
/// (fastest-varying, usually spatial) dimensions first.
pub fn chunk_shape(shape: &[u64], element_size: u64) -> Vec<u64> {
    let mut budget = (TARGET_CHUNK_BYTES / element_size.max(1)).max(1);
    let mut chunks = vec![1; shape.len()];
    for (d, &len) in shape.iter().enumerate().rev() {
        let len = len.max(1);
        let take = len.min(budget).max(1);
        chunks[d] = take;
        budget = (budget / take).max(1);
    }
    chunks
}

/// Write `array` as a single zarr v3 array at the store's root.
pub async fn write_array<
    T: MikroElement,
    S: AsyncReadableWritableStorageTraits + ?Sized + 'static,
>(
    store: Arc<S>,
    array: &ArrayD<T>,
    dimension_names: &[String],
) -> Result<(), MikroError> {
    if dimension_names.len() != array.ndim() {
        return Err(MikroError::Shape(format!(
            "{} dimension names for a {}-dimensional array",
            dimension_names.len(),
            array.ndim()
        )));
    }
    let shape: Vec<u64> = array.shape().iter().map(|&s| s as u64).collect();
    let chunks = chunk_shape(&shape, std::mem::size_of::<T>() as u64);

    let zarr = ArrayBuilder::new(shape, chunks, T::data_type(), T::default().into())
        .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(3, false))])
        .dimension_names(Some(dimension_names.iter().map(String::as_str)))
        .build(store, "/")?;
    zarr.async_store_metadata().await?;

    let (data, _) = array
        .as_standard_layout()
        .into_owned()
        .into_raw_vec_and_offset();
    zarr.async_store_array_subset(&zarr.subset_all(), data)
        .await?;
    Ok(())
}

/// Read the whole zarr array at the store's root.
pub async fn read_array<T: MikroElement, S: AsyncReadableStorageTraits + ?Sized + 'static>(
    store: Arc<S>,
) -> Result<ArrayD<T>, MikroError> {
    let zarr = Array::async_open(store, "/").await?;
    let data = zarr
        .async_retrieve_array_subset::<ArrayD<T>>(&zarr.subset_all())
        .await?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_through_an_object_store() {
        use object_store::{ObjectStore, ObjectStoreExt};
        let memory: Arc<dyn ObjectStore> = Arc::new(object_store::memory::InMemory::new());
        let store = Arc::new(zarrs_object_store::AsyncObjectStore::new(
            object_store::prefix::PrefixStore::new(memory.clone(), "grant-key"),
        ));
        let data = ndarray::Array::from_shape_fn(vec![2, 3, 4], |ix| {
            (ix[0] * 100 + ix[1] * 10 + ix[2]) as u16
        });
        let dims: Vec<String> = ["c", "y", "x"].iter().map(|s| s.to_string()).collect();
        write_array(store.clone(), &data, &dims).await.unwrap();
        let back: ArrayD<u16> = read_array(store.clone()).await.unwrap();
        assert_eq!(back, data);

        // A single array at the root of the granted prefix (no group, nothing above it).
        let meta = memory
            .get(&"grant-key/zarr.json".into())
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&meta).unwrap();
        assert_eq!(meta["node_type"], "array");
        assert_eq!(meta["dimension_names"], serde_json::json!(["c", "y", "x"]));
        assert!(memory.get(&"zarr.json".into()).await.is_err());

        let bad = write_array(store, &data, &dims[..2]).await;
        assert!(matches!(bad, Err(MikroError::Shape(_))));
    }

    #[test]
    fn chunks_fill_trailing_dims() {
        // 5D image of 1x1x10x1000x1000 u16: a whole 1000x1000 plane is 2 MB,
        // so ten planes (20 MB) fit into one chunk.
        assert_eq!(
            chunk_shape(&[1, 1, 10, 1000, 1000], 2),
            vec![1, 1, 10, 1000, 1000]
        );
        assert_eq!(
            chunk_shape(&[3, 100, 4096, 4096], 1),
            vec![1, 1, 4096, 4096]
        );
        assert_eq!(chunk_shape(&[0, 5], 4), vec![1, 5]);
    }
}
