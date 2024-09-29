use std::sync::Arc;

use bytes::Buf;
use http::{
    header::{CONTENT_LENGTH, RANGE},
    request::Builder,
    Method, Request,
};
use http_body_util::{BodyExt, Empty, Full};
use percent_encoding::utf8_percent_encode;

use super::{options::S3Options, STRICT_PATH_ENCODE_SET};
use crate::{
    buf::IoBufMut,
    path::Path,
    remotes::{
        aws::sign::Sign,
        http::{DynHttpClient, HttpClient as _},
    },
    Error, IoBuf, Read, Seek, Write,
};

pub struct S3File {
    options: Arc<S3Options>,
    path: Path,
    pos: u64,

    client: Arc<dyn DynHttpClient>,
}

impl S3File {
    pub(crate) fn new(options: Arc<S3Options>, path: Path, client: Arc<dyn DynHttpClient>) -> Self {
        Self {
            options,
            path,
            pos: 0,
            client,
        }
    }

    fn build_request(&self, method: Method) -> Builder {
        let url = format!(
            "{}/{}",
            self.options.endpoint,
            utf8_percent_encode(self.path.as_ref(), &STRICT_PATH_ENCODE_SET)
        );

        Request::builder().method(method).uri(url)
    }
}

impl Read for S3File {
    async fn read_exact<B: IoBufMut>(&mut self, mut buf: B) -> Result<B, Error> {
        let mut request = self
            .build_request(Method::GET)
            .header(
                RANGE,
                format!(
                    "bytes={}-{}",
                    self.pos,
                    self.pos + buf.as_slice().len() as u64 - 1
                ),
            )
            .body(Empty::new())?;
        request.sign(&self.options).await?;

        let response = self
            .client
            .send_request(request)
            .await
            .map_err(Error::Other)?;

        if !response.status().is_success() {
            Err(Error::Other(
                format!("failed to read from S3, HTTP status: {}", response.status()).into(),
            ))
        } else {
            std::io::copy(
                &mut response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| Error::Other(e.into()))?
                    .aggregate()
                    .reader(),
                &mut buf.as_slice_mut(),
            )?;

            self.pos += buf.as_slice().len() as u64;
            Ok(buf)
        }
    }

    async fn size(&self) -> Result<u64, Error> {
        let mut request = self.build_request(Method::HEAD).body(Empty::new())?;
        request.sign(&self.options).await?;

        let response = self
            .client
            .send_request(request)
            .await
            .map_err(Error::Other)?;

        if !response.status().is_success() {
            Err(Error::Other(
                format!(
                    "failed to get size from S3, HTTP status: {}",
                    response.status()
                )
                .into(),
            ))
        } else {
            let size = response
                .headers()
                .get(CONTENT_LENGTH)
                .ok_or_else(|| Error::Other("missing content-length header".into()))?
                .to_str()
                .map_err(|e| Error::Other(e.into()))?
                .parse::<u64>()
                .map_err(|e| Error::Other(e.into()))?;
            Ok(size)
        }
    }
}

impl Seek for S3File {
    async fn seek(&mut self, pos: u64) -> Result<(), Error> {
        self.pos = pos;
        Ok(())
    }
}

impl Write for S3File {
    async fn write_all<B: IoBuf>(&mut self, buf: B) -> (Result<(), Error>, B) {
        let request = self
            .build_request(Method::PUT)
            .header(CONTENT_LENGTH, buf.as_slice().len())
            .body(Full::new(buf.as_bytes()));
        if let Err(e) = request {
            return (Err(Error::Other(e.into())), buf);
        }
        let mut request = request.unwrap();

        let result = request.sign(&self.options).await;
        match result {
            Ok(_) => {
                let response = self.client.send_request(request).await;
                match response {
                    Ok(response) => {
                        if response.status().is_success() {
                            self.pos += buf.as_slice().len() as u64;
                            (Ok(()), buf)
                        } else {
                            (
                                Err(Error::Other(
                                    format!(
                                        "failed to write to S3, HTTP status: {} content: {}",
                                        response.status(),
                                        String::from_utf8_lossy(
                                            &response
                                                .into_body()
                                                .collect()
                                                .await
                                                .unwrap()
                                                .to_bytes()
                                        )
                                    )
                                    .into(),
                                )),
                                buf,
                            )
                        }
                    }
                    Err(e) => (Err(Error::Other(e)), buf),
                }
            }
            Err(e) => (Err(e), buf),
        }
    }

    async fn sync_data(&self) -> Result<(), Error> {
        Ok(())
    }

    async fn sync_all(&self) -> Result<(), Error> {
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "tokio-http", not(feature = "completion-based")))]
    #[tokio::test]
    async fn write_and_read_s3_file() {
        use std::{env, sync::Arc};

        use crate::{
            remotes::{
                aws::{credential::AwsCredential, options::S3Options, s3::S3File},
                http::tokio::TokioClient,
            },
            Read, Seek, Write,
        };

        if env::var("AWS_ACCESS_KEY_ID").is_err() {
            eprintln!("skipping AWS s3 test");
            return;
        }
        let key_id = env::var("AWS_ACCESS_KEY_ID").unwrap();
        let secret_key = env::var("AWS_SECRET_ACCESS_KEY").unwrap();

        let client = TokioClient::new();
        let region = "ap-southeast-1";
        let options = Arc::new(S3Options {
            endpoint: "https://fusio-test.s3.ap-southeast-1.amazonaws.com".into(),
            credential: Some(AwsCredential {
                key_id,
                secret_key,
                token: None,
            }),
            region: region.into(),
            sign_payload: true,
            checksum: false,
        });

        let client = Arc::new(client);

        let mut s3 = S3File::new(options, "test.txt".into(), client);

        let (result, _) = s3
            .write_all(&b"The answer of life, universe and everthing"[..])
            .await;
        result.unwrap();

        s3.seek(0).await.unwrap();

        let size = s3.size().await.unwrap();
        assert_eq!(size, 42);
        let buf = Vec::new();
        let buf = s3.read_to_end(buf).await.unwrap();
        assert_eq!(buf, b"The answer of life, universe and everthing");
    }
}
