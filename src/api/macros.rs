macro_rules! api_endpoint_methods {
    () => {};

    (list($name:ident, $request:ty, $response:ty, $path:literal, $context:literal); $($rest:tt)*) => {
        /// Lists resources in this API category.
        ///
        /// The request object is serialized as query parameters. Optional fields
        /// are omitted, and repeated fields use repeated query keys.
        pub async fn $name(&self, request: $request) -> crate::Result<$response> {
            self.api
                .get_json_with_query($path, &request, $context)
                .await
        }

        api_endpoint_methods!($($rest)*);
    };

    (get($name:ident, $id:ident, $response:ty, $path:literal, $context:literal); $($rest:tt)*) => {
        /// Fetches one resource by id.
        ///
        /// The id is URL-encoded before it is appended to the endpoint path.
        pub async fn $name(&self, $id: &str) -> crate::Result<$response> {
            self.api
                .get_json(
                    &format!("{}/{}", $path, $crate::api::client::path_segment($id)),
                    $context,
                )
                .await
        }

        api_endpoint_methods!($($rest)*);
    };

    (create($name:ident, $request:ty, $response:ty, $path:literal, $context:literal); $($rest:tt)*) => {
        /// Creates a resource in this API category.
        ///
        /// The request object is serialized as the JSON body for the endpoint.
        pub async fn $name(&self, request: $request) -> crate::Result<$response> {
            self.api.post_json($path, &request, $context).await
        }

        api_endpoint_methods!($($rest)*);
    };

    (post_child($name:ident, $id:ident, $request:ty, $response:ty, $path:literal, $child:literal, $context:literal); $($rest:tt)*) => {
        /// Calls a child endpoint for one resource using a JSON request body.
        ///
        /// The parent id is URL-encoded before it is appended to the endpoint
        /// path.
        pub async fn $name(&self, $id: &str, request: $request) -> crate::Result<$response> {
            self.api
                .post_json(
                    &format!(
                        "{}/{}/{}",
                        $path,
                        $crate::api::client::path_segment($id),
                        $child
                    ),
                    &request,
                    $context,
                )
                .await
        }

        api_endpoint_methods!($($rest)*);
    };

    (get_child($name:ident, $id:ident, $request:ty, $response:ty, $path:literal, $child:literal, $context:literal); $($rest:tt)*) => {
        /// Calls a child endpoint for one resource using query parameters.
        ///
        /// The parent id is URL-encoded before it is appended to the endpoint
        /// path.
        pub async fn $name(&self, $id: &str, request: $request) -> crate::Result<$response> {
            self.api
                .get_json_with_query(
                    &format!(
                        "{}/{}/{}",
                        $path,
                        $crate::api::client::path_segment($id),
                        $child
                    ),
                    &request,
                    $context,
                )
                .await
        }

        api_endpoint_methods!($($rest)*);
    };
}

macro_rules! api_getters {
    () => {};

    (str $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> &str {
            &self.$field
        }

        api_getters!($($rest)*);
    };

    (bool $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> bool {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (u64 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> u64 {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (u8 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> u8 {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (usize $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> usize {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (f64 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> f64 {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_bool $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<bool> {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_u32 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<u32> {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_u64 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<u64> {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_usize $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<usize> {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_f64 $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<f64> {
            self.$field
        }

        api_getters!($($rest)*);
    };

    (option_str $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<&str> {
            self.$field.as_deref()
        }

        api_getters!($($rest)*);
    };

    (ref $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` field.")]
        pub fn $field(&self) -> &$ty {
            &self.$field
        }

        api_getters!($($rest)*);
    };

    (option_ref $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` field.")]
        pub fn $field(&self) -> Option<&$ty> {
            self.$field.as_ref()
        }

        api_getters!($($rest)*);
    };

    (slice $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` collection.")]
        pub fn $field(&self) -> &[$ty] {
            &self.$field
        }

        api_getters!($($rest)*);
    };

    (option_slice $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` collection.")]
        pub fn $field(&self) -> Option<&[$ty]> {
            self.$field.as_deref()
        }

        api_getters!($($rest)*);
    };

    (map $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` object.")]
        pub fn $field(&self) -> &serde_json::Map<String, serde_json::Value> {
            &self.$field
        }

        api_getters!($($rest)*);
    };

    (option_map $field:ident; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` object.")]
        pub fn $field(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
            self.$field.as_ref()
        }

        api_getters!($($rest)*);
    };

    (hash_map $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the `", stringify!($field), "` map.")]
        pub fn $field(&self) -> &std::collections::HashMap<String, $ty> {
            &self.$field
        }

        api_getters!($($rest)*);
    };

    (option_hash_map $field:ident: $ty:ty; $($rest:tt)*) => {
        #[doc = concat!("Returns the optional `", stringify!($field), "` map.")]
        pub fn $field(&self) -> Option<&std::collections::HashMap<String, $ty>> {
            self.$field.as_ref()
        }

        api_getters!($($rest)*);
    };
}
