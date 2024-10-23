use std::pin::Pin;

use futures_core::Stream;

use super::MaybeSendFuture;
use crate::{
    buf::IoBufMut,
    fs::{FileMeta, Fs, OpenOptions},
    path::Path,
    DynRead, DynWrite, Error, IoBuf, MaybeSend, MaybeSync, Read, Write,
};

pub trait DynFile: DynRead + DynWrite + 'static {}

impl<F> DynFile for F where F: DynRead + DynWrite + 'static {}

impl<'read> Read for Box<dyn DynFile + 'read> {
    async fn read_exact_at<B: IoBufMut>(&mut self, buf: B, pos: u64) -> (Result<(), Error>, B) {
        let (result, buf) =
            DynRead::read_exact_at(self.as_mut(), unsafe { buf.slice_mut_unchecked(..) }, pos)
                .await;
        (result, unsafe { B::recover_from_slice_mut(buf) })
    }

    async fn read_to_end_at(&mut self, buf: Vec<u8>, pos: u64) -> (Result<(), Error>, Vec<u8>) {
        DynRead::read_to_end_at(self.as_mut(), buf, pos).await
    }

    async fn size(&self) -> Result<u64, Error> {
        DynRead::size(self.as_ref()).await
    }
}

impl<'write> Write for Box<dyn DynFile + 'write> {
    async fn write_all<B: IoBuf>(&mut self, buf: B) -> (Result<(), Error>, B) {
        let (result, buf) =
            DynWrite::write_all(self.as_mut(), unsafe { buf.slice_unchecked(..) }).await;
        (result, unsafe { B::recover_from_slice(buf) })
    }

    async fn flush(&mut self) -> Result<(), Error> {
        DynWrite::flush(self.as_mut()).await
    }

    async fn close(&mut self) -> Result<(), Error> {
        DynWrite::close(self.as_mut()).await
    }
}

pub trait DynFs: MaybeSend + MaybeSync {
    fn open<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<Box<dyn DynFile>, Error>> + 's>> {
        self.open_options(path, OpenOptions::default())
    }

    fn open_options<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
        options: OpenOptions,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<Box<dyn DynFile>, Error>> + 's>>;

    fn create_dir_all<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<(), Error>> + 's>>;

    fn list<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<
        Box<
            dyn MaybeSendFuture<
                    Output = Result<
                        Pin<Box<dyn Stream<Item = Result<FileMeta, Error>> + 's>>,
                        Error,
                    >,
                > + 's,
        >,
    >;

    fn remove<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<(), Error>> + 's>>;
}

impl<F: Fs> DynFs for F {
    fn open_options<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
        options: OpenOptions,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<Box<dyn DynFile>, Error>> + 's>> {
        Box::pin(async move {
            let file = F::open_options(self, path, options).await?;
            Ok(Box::new(file) as Box<dyn DynFile>)
        })
    }

    fn create_dir_all<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<(), Error>> + 's>> {
        Box::pin(F::create_dir_all(path))
    }

    fn list<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<
        Box<
            dyn MaybeSendFuture<
                    Output = Result<
                        Pin<Box<dyn Stream<Item = Result<FileMeta, Error>> + 's>>,
                        Error,
                    >,
                > + 's,
        >,
    > {
        Box::pin(async move {
            let stream = F::list(self, path).await?;
            Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = Result<FileMeta, Error>>>>)
        })
    }

    fn remove<'s, 'path: 's>(
        &'s self,
        path: &'path Path,
    ) -> Pin<Box<dyn MaybeSendFuture<Output = Result<(), Error>> + 's>> {
        Box::pin(F::remove(self, path))
    }
}

#[cfg(test)]
mod tests {

    #[cfg(all(feature = "tokio", not(feature = "completion-based")))]
    #[tokio::test]
    async fn test_dyn_fs() {
        use tempfile::tempfile;

        use crate::Write;

        let file = tokio::fs::File::from_std(tempfile().unwrap());
        let mut dyn_file: Box<dyn super::DynFile> = Box::new(file);
        let buf = [24, 9, 24, 0];
        let (result, _) = dyn_file.write_all(&buf[..]).await;
        result.unwrap();
    }
}
