/*
 * Copyright 2015 Ben Ashford
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#![crate_type = "lib"]
#![crate_name = "rs_es"]

//! A client for ElasticSearch's REST API
//!
//! The `Client` itself is used as the central access point, from which numerous
//! operations are defined implementing each of the specific ElasticSearch APIs.
//!
//! Warning: at the time of writing the majority of such APIs are currently
//! unimplemented.

#[macro_use]
extern crate log;
extern crate hyper;
extern crate rustc_serialize;

#[macro_use]
pub mod util;

pub mod error;
pub mod operations;
pub mod query;
pub mod units;

use hyper::status::StatusCode;

use rustc_serialize::Encodable;
use rustc_serialize::json::{self, Json};

use error::EsError;
use operations::bulk::{BulkOperation, Action};
use operations::delete::{DeleteOperation, DeleteByQueryOperation};
use operations::get::GetOperation;
use operations::index::IndexOperation;
use operations::search::{SearchURIOperation, SearchQueryOperation};
use operations::RefreshOperation;
use operations::analyze::AnalyzeOperation;

// The client

/// Process the result of an HTTP request, returning the status code and the
/// `Json` result (if the result had a body) or an `EsError` if there were any
/// errors
///
/// This function is exposed to allow extensions to certain operations, it is
/// not expected to be used by consumers of the library
pub fn do_req(resp: &mut hyper::client::response::Response)
              -> Result<(StatusCode, Option<Json>), EsError> {
    info!("Response: {:?}", resp);
    match resp.status {
        StatusCode::Ok |
        StatusCode::Created |
        StatusCode::NotFound => match Json::from_reader(resp) {
            Ok(json) => Ok((resp.status, Some(json))),
            Err(e)   => Err(EsError::from(e))
        },
        _                    => Err(EsError::from(resp))
    }
}

/// The core of the ElasticSearch client, owns a HTTP connection.
///
/// Each instance of `Client` is reusable, but only one thread can use each one
/// at once.  This will be enforced by the borrow-checker as most methods are
/// defined on `&mut self`.
///
/// To create a `Client`, the hostname and port need to be specified.
///
/// Each ElasticSearch API operation is defined as a method on `Client`.  Any
/// compulsory parameters must be given as arguments to this method.  It returns
/// an operation builder that can be used to add any optional parameters.
///
/// Finally `send` is called to submit the operation:
///
/// # Examples
///
/// ```
/// use rs_es::Client;
///
/// let mut client = Client::new("localhost", 9200);
/// let result = client.search_uri()
///                    .with_indexes(&["index_name"])
///                    .with_query("field:value")
///                    .send();
/// ```
///
/// See the specific operations and their builder objects for details.
pub struct Client {
    base_url:    String,
    http_client: hyper::Client
}

/// Create a HTTP function for the given method (GET/PUT/POST/DELETE)
macro_rules! es_op {
    ($n:ident,$cn:ident) => {
        fn $n(&mut self, url: &str)
              -> Result<(StatusCode, Option<Json>), EsError> {
            info!("Doing {} on {}", stringify!($n), url);
            let url = self.full_url(url);
            let mut result = try!(self.http_client.$cn(&url).send());
            do_req(&mut result)
        }
    }
}

/// Create a HTTP function with a request body for the given method
/// (GET/PUT/POST/DELETE)
macro_rules! es_body_op {
    ($n:ident,$cn:ident) => {
        fn $n<E>(&mut self, url: &str, body: &E)
                 -> Result<(StatusCode, Option<Json>), EsError>
            where E: Encodable {
                info!("Doing {} on {}", stringify!($n), url);
                let json_string = try!(json::encode(body));
                info!("Body: {}", json_string);
                let url = self.full_url(url);
                let mut result = try!(self.http_client
                                      .$cn(&url)
                                      .body(&json_string)
                                      .send());

                do_req(&mut result)
            }
    }
}

impl Client {
    /// Create a new client
    pub fn new(host: &str, port: u32) -> Client {
        Client {
            base_url:    format!("http://{}:{}", host, port),
            http_client: hyper::Client::new()
        }
    }

    /// Take a nearly complete ElasticSearch URL, and stick the host/port part
    /// on the front.
    pub fn full_url(&self, suffix: &str) -> String {
        format!("{}/{}", self.base_url, suffix)
    }

    es_op!(get_op, get);

    es_op!(post_op, post);
    es_body_op!(post_body_op, post);
    es_op!(put_op, put);
    es_body_op!(put_body_op, put);
    es_op!(delete_op, delete);
    es_body_op!(delete_body_op, delete);

    /// Calls the base ES path, returning the version number
    pub fn version(&mut self) -> Result<String, EsError> {
        let (_, result) = try!(self.get_op("/"));
        let json = result.expect("No Json payload");
        match json.find_path(&["version", "number"]) {
            Some(version) => match version.as_string() {
                Some(string) => Ok(string.to_owned()),
                None         => Err(EsError::EsError(format!("Cannot find version number in: {:?}",
                                                             json)))
            },
            None          => Err(EsError::EsError(format!("Cannot find version number in {:?}",
                                                          json)))
        }
    }

    // Indices APIs

    /// Refresh
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/indices-refresh.html
    pub fn refresh<'a>(&'a mut self) -> RefreshOperation {
        RefreshOperation::new(self)
    }

    // Document APIs

    /// An index operation to index a document in the specified index.
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/docs-index_.html
    pub fn index<'a, 'b, E: Encodable>(&'a mut self, index: &'b str, doc_type: &'b str)
                                       -> IndexOperation<'a, 'b, E> {
        IndexOperation::new(self, index, doc_type)
    }

    /// Implementation of the ES GET API
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/docs-get.html
    pub fn get<'a>(&'a mut self,
                   index: &'a str,
                   id:    &'a str) -> GetOperation {
        GetOperation::new(self, index, id)
    }

    /// Delete by ID
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/docs-delete.html
    pub fn delete<'a>(&'a mut self,
                      index:    &'a str,
                      doc_type: &'a str,
                      id:       &'a str) -> DeleteOperation {
        DeleteOperation::new(self, index, doc_type, id)
    }

    // Multi-document APIs

    /// Delete by query
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/docs-delete-by-query.html
    ///
    /// Warning: will be removed in ElasticSearch 2.0
    pub fn delete_by_query<'a>(&'a mut self) -> DeleteByQueryOperation {
        DeleteByQueryOperation::new(self)
    }

    /// Bulk
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/current/docs-bulk.html
    pub fn bulk<'a, 'b>(&'a mut self, actions: &'b [Action]) -> BulkOperation<'a, 'b> {
        BulkOperation::new(self, actions)
    }

    /// Analyze
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/current/indices-analyze.html
    pub fn analyze<'a>(&'a mut self,
                       body: &'a str) -> AnalyzeOperation {
        AnalyzeOperation::new(self, body)
    }

    // Search APIs

    /// Search via the query parameter
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/search-uri-request.html
    pub fn search_uri<'a>(&'a mut self) -> SearchURIOperation {
        SearchURIOperation::new(self)
    }

    /// Search via the query DSL
    ///
    /// See: https://www.elastic.co/guide/en/elasticsearch/reference/1.x/search-request-body.html
    pub fn search_query<'a>(&'a mut self) -> SearchQueryOperation {
        SearchQueryOperation::new(self)
    }
}

#[cfg(test)]
pub mod tests {
    extern crate env_logger;
    extern crate regex;

    use std::collections::BTreeMap;
    use std::env;

    use rustc_serialize::json::{Json, ToJson};

    use super::Client;
    use super::operations::bulk::Action;
    use super::operations::index::OpType;

    use super::query::{Filter, Query};

    use self::regex::Regex;

    // test setup

    pub fn make_client() -> Client {
        let hostname = match env::var("ES_HOST") {
            Ok(val) => val,
            Err(_)  => "localhost".to_owned()
        };
        Client::new(&hostname, 9200)
    }

    #[derive(Debug, RustcDecodable, RustcEncodable)]
    pub struct TestDocument {
        pub str_field: String,
        pub int_field: i64
    }

    impl TestDocument {
        pub fn new() -> TestDocument {
            TestDocument {
                str_field: "I am a test".to_owned(),
                int_field: 1
            }
        }

        pub fn with_str_field(mut self, s: &str) -> TestDocument {
            self.str_field = s.to_owned();
            self
        }

        pub fn with_int_field(mut self, i: i64) -> TestDocument {
            self.int_field = i;
            self
        }
    }

    impl ToJson for TestDocument {
        fn to_json(&self) -> Json {
            let mut d = BTreeMap::new();
            d.insert("str_field".to_owned(), self.str_field.to_json());
            d.insert("int_field".to_owned(), self.int_field.to_json());
            Json::Object(d)
        }
    }

    pub fn clean_db(client: &mut Client,
                    test_idx: &str) {
        client.delete_by_query()
            .with_indexes(&[test_idx])
            .with_query(&Query::build_match_all().build())
            .send()
            .unwrap();
    }

    // tests

    #[test]
    fn it_works() {
        env_logger::init().unwrap();

        let mut client = make_client();
        let result = client.version().unwrap();

        let expected_regex = Regex::new(r"^\d\.\d\.\d$").unwrap();
        assert_eq!(expected_regex.is_match(&result), true);
    }

    #[test]
    fn test_indexing() {
        let index_name = "test_indexing";
        let mut client = make_client();
        clean_db(&mut client, index_name);
        {
            let result_wrapped = client
                .index(index_name, "test_type")
                .with_doc(&TestDocument::new().with_int_field(1))
                .with_ttl(927500)
                .send();
            info!("TEST RESULT: {:?}", result_wrapped);
            let result = result_wrapped.unwrap();
            assert_eq!(result.created, true);
            assert_eq!(result.index, index_name);
            assert_eq!(result.doc_type, "test_type");
            assert!(result.id.len() > 0);
            assert_eq!(result.version, 1);
        }
        {
            let delete_result = client.delete(index_name, "test_type", "TEST_INDEXING_2").send();
            info!("DELETE RESULT: {:?}", delete_result);

            let result_wrapped = client
                .index(index_name, "test_type")
                .with_doc(&TestDocument::new().with_int_field(2))
                .with_id("TEST_INDEXING_2")
                .with_op_type(OpType::Create)
                .send();
            let result = result_wrapped.unwrap();

            assert_eq!(result.created, true);
            assert_eq!(result.index, index_name);
            assert_eq!(result.doc_type, "test_type");
            assert_eq!(result.id, "TEST_INDEXING_2");
            assert!(result.version >= 1);
        }
    }

    #[test]
    fn test_get() {
        let index_name = "test_get";
        let mut client = make_client();
        clean_db(&mut client, index_name);
        {
            let doc = TestDocument::new().with_int_field(3);
            client
                .index(index_name, "test_type")
                .with_id("TEST_GETTING")
                .with_doc(&doc)
                .send().unwrap();
        }
        {
            let mut getter = client.get(index_name, "TEST_GETTING");
            let result_wrapped = getter
                .with_doc_type("test_type")
                .send();
            info!("RESULT: {:?}", result_wrapped);
            let result = result_wrapped.unwrap();
            assert_eq!(result.id, "TEST_GETTING");

            let source:TestDocument = result.source().unwrap();
            assert_eq!(source.str_field, "I am a test");
            assert_eq!(source.int_field, 3);
        }
    }

    #[test]
    fn test_delete_by_query() {
        let index_name = "test_delete_by_query";
        let mut client = make_client();
        clean_db(&mut client, index_name);

        let td1 = TestDocument::new().with_str_field("TEST DOC 1").with_int_field(100);
        let td2 = TestDocument::new().with_str_field("TEST DOC 2").with_int_field(200);

        client
            .index(index_name, "test_type")
            .with_id("ABC123")
            .with_doc(&td1)
            .send().unwrap();
        client
            .index(index_name, "test_type")
            .with_id("ABC124")
            .with_doc(&td2)
            .send().unwrap();

        let delete_result = client
            .delete_by_query()
            .with_indexes(&[index_name])
            .with_doc_types(&["test_type"])
            .with_query(&Query::build_match("int_field", 200)
                        .with_lenient(false)
                        .build())
            .send().unwrap();

        assert!(delete_result.unwrap().successful());

        let doc1 = client.get(index_name, "ABC123").with_doc_type("test_type").send().unwrap();
        let doc2 = client.get(index_name, "ABC124").with_doc_type("test_type").send().unwrap();

        assert!(doc1.found);
        assert!(!doc2.found);
    }

    fn setup_search_test_data(client: &mut Client, index_name: &str) {
        let documents = vec![
            TestDocument::new().with_str_field("Document A123").with_int_field(1),
            TestDocument::new().with_str_field("Document B456").with_int_field(2),
            TestDocument::new().with_str_field("Document 1ABC").with_int_field(3)
                ];
        for ref doc in documents {
            client.index(index_name, "test_type")
                .with_doc(doc)
                .send()
                .unwrap();
        }
        client.refresh().with_indexes(&[index_name]).send().unwrap();
    }

    #[test]
    fn test_search_uri() {
        let index_name = "test_search_uri";
        let mut client = make_client();
        clean_db(&mut client, index_name);
        setup_search_test_data(&mut client, index_name);

        let all_results = client.search_uri().with_indexes(&[index_name]).send().unwrap();
        assert_eq!(3, all_results.hits.total);

        let doc_a = client
            .search_uri()
            .with_indexes(&[index_name])
            .with_query("A123")
            .send()
            .unwrap();
        assert_eq!(1, doc_a.hits.total);

        let doc_1 = client
            .search_uri()
            .with_indexes(&[index_name])
            .with_query("str_field:1ABC")
            .send()
            .unwrap();
        assert_eq!(1, doc_1.hits.total);

        let limited_fields = client
            .search_uri()
            .with_indexes(&[index_name])
            .with_query("str_field:B456")
            .with_fields(&["int_field"])
            .send()
            .unwrap();
        assert_eq!(1, limited_fields.hits.total);
    }

    #[test]
    fn test_search_body() {
        let index_name = "test_search_body";
        let mut client = make_client();
        clean_db(&mut client, index_name);
        setup_search_test_data(&mut client, index_name);

        let all_results = client
            .search_query()
            .with_indexes(&[index_name])
            .with_query(&Query::build_match_all().build())
            .send().unwrap();
        assert_eq!(3, all_results.hits.total);

        let within_range = client
            .search_query()
            .with_indexes(&[index_name])
            .with_query(&Query::build_filtered(Filter::build_range("int_field")
                                               .with_gte(2)
                                               .with_lte(3)
                                               .build())
                        .build())
            .send().unwrap();
        assert_eq!(2, within_range.hits.total);
    }

    #[test]
    fn test_bulk() {
        let index_name = "test_bulk";
        let mut client = make_client();
        clean_db(&mut client, index_name);

        let actions:Vec<Action> = (1..10).map(|i| {
            let doc = TestDocument::new().with_str_field("bulk_doc").with_int_field(i);
            Action::index(doc)
        }).collect();

        let result = client.bulk(&actions)
            .with_index(index_name)
            .with_doc_type("bulk_type")
            .send()
            .unwrap();

        assert_eq!(false, result.errors);
        assert_eq!(9, result.items.len());
    }
}
