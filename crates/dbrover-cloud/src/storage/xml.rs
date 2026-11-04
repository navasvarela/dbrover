//! The XML S3 answers a listing with, and refuses one with.

use roxmltree::{Document, Node};

use super::{Level, StorageError, StoredFile};

/// A `ListBucketResult`: its common prefixes as folders, its contents as files, and its
/// continuation token when it is truncated.
pub(super) fn s3_level(body: &[u8]) -> Result<Level, StorageError> {
    let text = std::str::from_utf8(body).map_err(|error| unreadable(&error))?;
    let document = Document::parse(text).map_err(|error| unreadable(&error))?;
    let root = document.root_element();
    if root.tag_name().name() != "ListBucketResult" {
        return Err(StorageError::Unreadable(format!(
            "expected ListBucketResult, found {}",
            root.tag_name().name()
        )));
    }
    let folders = children(root, "CommonPrefixes")
        .filter_map(|prefixes| text_of(prefixes, "Prefix"))
        .map(str::to_owned)
        .collect();
    let files = children(root, "Contents")
        .map(|contents| {
            let key = text_of(contents, "Key")
                .ok_or_else(|| StorageError::Unreadable("an object without a key".to_owned()))?;
            let size = text_of(contents, "Size")
                .and_then(|size| size.parse().ok())
                .unwrap_or_default();
            Ok(StoredFile {
                key: key.to_owned(),
                size,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    let truncated = text_of(root, "IsTruncated") == Some("true");
    let next = truncated
        .then(|| text_of(root, "NextContinuationToken"))
        .flatten()
        .map(str::to_owned);
    Ok(Level {
        folders,
        files,
        next,
    })
}

/// List Blobs' `EnumerationResults`: its blob prefixes as folders, its blobs as files, and
/// its next marker, which is empty when the level is whole.
pub(super) fn azure_level(body: &[u8]) -> Result<Level, StorageError> {
    let text = std::str::from_utf8(body).map_err(|error| unreadable(&error))?;
    // Azure starts its answers with a byte-order mark, which isn't XML.
    let document =
        Document::parse(text.trim_start_matches('\u{feff}')).map_err(|error| unreadable(&error))?;
    let root = document.root_element();
    if root.tag_name().name() != "EnumerationResults" {
        return Err(StorageError::Unreadable(format!(
            "expected EnumerationResults, found {}",
            root.tag_name().name()
        )));
    }
    let blobs = children(root, "Blobs").next();
    let folders = blobs
        .into_iter()
        .flat_map(|blobs| children(blobs, "BlobPrefix"))
        .filter_map(|prefix| text_of(prefix, "Name"))
        .map(str::to_owned)
        .collect();
    let files = blobs
        .into_iter()
        .flat_map(|blobs| children(blobs, "Blob"))
        .map(|blob| {
            let key = text_of(blob, "Name")
                .ok_or_else(|| StorageError::Unreadable("a blob without a name".to_owned()))?;
            let size = children(blob, "Properties")
                .next()
                .and_then(|properties| text_of(properties, "Content-Length"))
                .and_then(|size| size.parse().ok())
                .unwrap_or_default();
            Ok(StoredFile {
                key: key.to_owned(),
                size,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    let next = text_of(root, "NextMarker")
        .filter(|marker| !marker.is_empty())
        .map(str::to_owned);
    Ok(Level {
        folders,
        files,
        next,
    })
}

/// S3's `Error` document (Azure's has the same `Code` and `Message`), or the status alone when the answer has none (a `HEAD`, a proxy's
/// page).
pub(super) fn s3_refusal(status: u16, body: &[u8]) -> StorageError {
    let document = std::str::from_utf8(body)
        .ok()
        .and_then(|text| Document::parse(text).ok());
    let error = document.as_ref().map(Document::root_element);
    let field = |name| error.and_then(|error| text_of(error, name));
    StorageError::Refused {
        status,
        code: field("Code").unwrap_or("NoCode").to_owned(),
        message: field("Message")
            .unwrap_or("the store refused to list the bucket")
            .to_owned(),
    }
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

fn text_of<'a>(node: Node<'a, '_>, name: &'static str) -> Option<&'a str> {
    children(node, name).next()?.text()
}

fn unreadable(error: &dyn std::error::Error) -> StorageError {
    StorageError::Unreadable(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As S3 documents `ListObjectsV2`'s answer, with a delimiter.
    const TRUNCATED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>sales</Name>
  <Prefix>2026/</Prefix>
  <KeyCount>3</KeyCount>
  <MaxKeys>3</MaxKeys>
  <Delimiter>/</Delimiter>
  <IsTruncated>true</IsTruncated>
  <Contents>
    <Key>2026/orders.parquet</Key>
    <LastModified>2026-11-01T10:00:00.000Z</LastModified>
    <ETag>"6805f2cfc46c0f04559748bb039d69ae"</ETag>
    <Size>4096</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
  <CommonPrefixes><Prefix>2026/01/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>2026/02/</Prefix></CommonPrefixes>
  <NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>
</ListBucketResult>"#;

    #[test]
    fn reads_a_levels_folders_files_and_continuation() {
        let level = s3_level(TRUNCATED.as_bytes()).unwrap();

        assert_eq!(level.folders, ["2026/01/", "2026/02/"]);
        assert_eq!(
            level.files,
            [StoredFile {
                key: "2026/orders.parquet".to_owned(),
                size: 4096,
            }]
        );
        assert_eq!(
            level.next.as_deref(),
            Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=")
        );
    }

    #[test]
    fn a_whole_level_has_no_continuation() {
        let whole = TRUNCATED.replace(
            "<IsTruncated>true</IsTruncated>",
            "<IsTruncated>false</IsTruncated>",
        );

        assert_eq!(s3_level(whole.as_bytes()).unwrap().next, None);
    }

    #[test]
    fn a_refusal_carries_s3s_code_and_message() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>AccessDenied</Code><Message>Access Denied</Message><RequestId>1</RequestId></Error>"#;
        let cases = [
            (body.as_bytes(), "Access Denied (AccessDenied, HTTP 403)"),
            (
                b"<html>Bad gateway</html>".as_slice(),
                "the store refused to list the bucket (NoCode, HTTP 403)",
            ),
            (
                b"".as_slice(),
                "the store refused to list the bucket (NoCode, HTTP 403)",
            ),
        ];
        for (body, expected) in cases {
            assert_eq!(s3_refusal(403, body).to_string(), expected);
        }
    }

    /// As Azure documents List Blobs' answer, with a delimiter and a marker.
    const AZURE: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>
<EnumerationResults ServiceEndpoint=\"https://acme.blob.core.windows.net/\" ContainerName=\"raw\">
  <Prefix>2026/</Prefix>
  <MaxResults>2</MaxResults>
  <Delimiter>/</Delimiter>
  <Blobs>
    <Blob>
      <Name>2026/orders.parquet</Name>
      <Properties><Content-Length>4096</Content-Length><BlobType>BlockBlob</BlobType></Properties>
    </Blob>
    <BlobPrefix><Name>2026/01/</Name></BlobPrefix>
  </Blobs>
  <NextMarker>2!96!MDAwMDE5</NextMarker>
</EnumerationResults>";

    #[test]
    fn reads_an_azure_levels_prefixes_blobs_and_marker() {
        let level = azure_level(AZURE.as_bytes()).unwrap();

        assert_eq!(level.folders, ["2026/01/"]);
        assert_eq!(
            level.files,
            [StoredFile {
                key: "2026/orders.parquet".to_owned(),
                size: 4096,
            }]
        );
        assert_eq!(level.next.as_deref(), Some("2!96!MDAwMDE5"));
        let whole = AZURE.replace("<NextMarker>2!96!MDAwMDE5</NextMarker>", "<NextMarker />");
        assert_eq!(azure_level(whole.as_bytes()).unwrap().next, None);
    }

    #[test]
    fn an_answer_that_isnt_a_listing_is_unreadable() {
        let error = s3_level(b"<Other/>").unwrap_err();

        assert!(
            matches!(&error, StorageError::Unreadable(message) if message.contains("Other")),
            "{error}"
        );
    }
}
