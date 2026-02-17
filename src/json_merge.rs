//! Deep JSON merge implementation copied from brainstore/util/src/json.rs
//!
//! This is the canonical implementation used by brainstore for merging WAL entries.
//! It should be kept in sync with the brainstore implementation.

use std::{collections::HashSet, sync::LazyLock};

use serde_json::{map::Entry, Map, Value};

/// Fields that automatically use set-union merge semantics (unless in stop_deep_merge_paths).
static SET_UNION_FIELDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    s.insert("tags");
    s
});

/// Deep-merge the `from` value into the `into` value. The rules of deep merging are as follows:
/// - If both `into` and `from` are objects, then recursively deep-merge values between common keys.
/// - Otherwise, replace the `into` value with the `from` value.
pub fn deep_merge(into: &mut Value, from: &Value) {
    deep_merge_helper(into, from, None, true);
}

#[allow(dead_code)]
pub fn deep_merge_maps(into: &mut Map<String, Value>, from: &Map<String, Value>) {
    deep_merge_maps_helper(into, from, None, true);
}

// These are the same as the deep merge functions above, but they accept a `stop_deep_merge_paths`
// set, which is a set of paths that should not be deep merged beyond.
//
// E.g. if the `stop_deep_merge_paths` set contains the path ["a", "b", "c"], then if you are
// merging the following objects: into={"a": {"b": {"c": {"d": 1}}}}, from={"a": {"b": {"c": {"e":
// 2}}}}, then the resulting object will be {"a": {"b": {"c": {"e": 2}}}}.
#[allow(dead_code)]
pub fn deep_merge_with_stop_paths(
    into: &mut Value,
    from: &Value,
    stop_deep_merge_paths: &HashSet<Vec<&str>>,
) {
    deep_merge_helper(
        into,
        from,
        Some(MergePathsState {
            cur_path: vec![],
            stop_deep_merge_paths,
        }),
        true,
    );
}

#[allow(dead_code)]
pub fn deep_merge_maps_with_stop_paths(
    into: &mut Map<String, Value>,
    from: &Map<String, Value>,
    stop_deep_merge_paths: &HashSet<Vec<&str>>,
) {
    deep_merge_maps_helper(
        into,
        from,
        Some(MergePathsState {
            cur_path: vec![],
            stop_deep_merge_paths,
        }),
        true,
    );
}

struct MergePathsState<'a> {
    cur_path: Vec<&'a str>,
    stop_deep_merge_paths: &'a HashSet<Vec<&'a str>>,
}

fn deep_merge_helper<'a>(
    into: &mut Value,
    from: &'a Value,
    merge_paths_state: Option<MergePathsState<'a>>,
    is_top_level: bool,
) -> Option<MergePathsState<'a>> {
    match (into, from) {
        (Value::Object(into_obj), Value::Object(from_obj)) => {
            deep_merge_maps_helper(into_obj, from_obj, merge_paths_state, is_top_level)
        }
        (into, from) => {
            *into = from.clone();
            merge_paths_state
        }
    }
}

fn deep_merge_maps_helper<'a>(
    into: &mut Map<String, Value>,
    from: &'a Map<String, Value>,
    mut merge_paths_state: Option<MergePathsState<'a>>,
    is_top_level: bool,
) -> Option<MergePathsState<'a>> {
    for (key, from_value) in from.iter() {
        // Check if this path is in stop_deep_merge_paths (only if we have state)
        let is_stop_path = if let Some(ref mut state) = merge_paths_state {
            state.cur_path.push(key);
            state.stop_deep_merge_paths.contains(&state.cur_path)
        } else {
            false
        };

        // Check if this field should use set-union merge
        let is_set_union_field =
            is_top_level && SET_UNION_FIELDS.contains(key.as_str()) && !is_stop_path;

        match into.entry(key.clone()) {
            Entry::Occupied(mut into_entry) => {
                // Handle set-union merge for arrays (e.g., tags)
                if is_set_union_field {
                    if let (Value::Array(into_arr), Value::Array(from_arr)) =
                        (into_entry.get_mut(), from_value)
                    {
                        // Set-union merge: add elements from from_arr that aren't already in into_arr
                        // Assumes input arrays are already deduplicated
                        let existing: HashSet<String> =
                            into_arr.iter().map(|v| v.to_string()).collect();
                        for item in from_arr {
                            if !existing.contains(&item.to_string()) {
                                into_arr.push(item.clone());
                            }
                        }
                    } else {
                        // Not both arrays, fall back to replacement
                        *into_entry.get_mut() = from_value.clone();
                    }
                } else if !is_stop_path {
                    // Normal deep merge
                    merge_paths_state = deep_merge_helper(
                        into_entry.get_mut(),
                        from_value,
                        merge_paths_state,
                        false,
                    );
                } else {
                    // Stop path: replace entirely
                    *into_entry.get_mut() = from_value.clone();
                }
            }
            Entry::Vacant(into_entry) => {
                into_entry.insert(from_value.clone());
            }
        }

        if let Some(ref mut state) = merge_paths_state {
            state.cur_path.pop();
        }
    }
    merge_paths_state
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deep_merge_objects() {
        let mut into = json!({
            "a": 1,
            "b": {
                "c": 2,
                "d": 3
            }
        });
        let from = json!({
            "b": {
                "c": 4,
                "e": 5
            },
            "f": 6
        });
        deep_merge(&mut into, &from);
        assert_eq!(
            into,
            json!({
                "a": 1,
                "b": {
                    "c": 4,
                    "d": 3,
                    "e": 5
                },
                "f": 6
            })
        );
    }

    #[test]
    fn test_deep_merge_arrays() {
        let mut into = json!([1, 2, 3]);
        let from = json!([4, 5, 6]);
        deep_merge(&mut into, &from);
        assert_eq!(into, json!([4, 5, 6]));
    }

    #[test]
    fn test_deep_merge_primitives() {
        let mut into = json!(1);
        let from = json!(2);
        deep_merge(&mut into, &from);
        assert_eq!(into, json!(2));
    }

    #[test]
    fn test_deep_merge_mixed_types() {
        let mut into = json!({
            "a": 1,
            "b": [1, 2, 3],
            "c": {
                "d": 4
            }
        });
        let from = json!({
            "b": {
                "e": 5
            },
            "c": [6, 7, 8]
        });
        deep_merge(&mut into, &from);
        assert_eq!(
            into,
            json!({
                "a": 1,
                "b": {
                    "e": 5
                },
                "c": [6, 7, 8]
            })
        );
    }

    #[test]
    fn test_deep_merge_null_values() {
        let mut into = json!({
            "a": 1,
            "b": null
        });
        let from = json!({
            "b": 2,
            "c": null
        });
        deep_merge(&mut into, &from);
        assert_eq!(
            into,
            json!({
                "a": 1,
                "b": 2,
                "c": null
            })
        );
    }

    #[test]
    fn test_deep_merge_tags_set_union() {
        // Test 1: Basic tags set-union merge (tags at top level get set-union semantics)
        let mut into = json!({
            "tags": ["a", "b"],
            "other": "value"
        });
        let from = json!({
            "tags": ["b", "c", "d"],
            "other": "new_value"
        });
        deep_merge(&mut into, &from);
        // Tags should be combined with deduplication, other fields replaced
        let tags: Vec<&str> = into["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(tags.len(), 4); // a, b, c, d (b deduplicated)
        assert!(tags.contains(&"a"));
        assert!(tags.contains(&"b"));
        assert!(tags.contains(&"c"));
        assert!(tags.contains(&"d"));
        assert_eq!(into["other"], "new_value");
    }

    #[test]
    fn test_deep_merge_tags_empty_cases() {
        // Test: When into has no tags, from's tags are added
        let mut into = json!({"other": "value"});
        let from = json!({"tags": ["a", "b"]});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], json!(["a", "b"]));

        // Test: When from has empty tags array (set-union: no new elements to add)
        let mut into = json!({"tags": ["a", "b"]});
        let from = json!({"tags": []});
        deep_merge(&mut into, &from);
        // Empty from array means just keep what's in into (no new elements to add)
        assert_eq!(into["tags"], json!(["a", "b"]));

        // Test: When into has empty tags array
        let mut into = json!({"tags": []});
        let from = json!({"tags": ["a", "b"]});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], json!(["a", "b"]));

        // Test: When from has no tags field at all, into's tags are preserved
        let mut into = json!({"tags": ["a", "b"], "other": "old"});
        let from = json!({"other": "new"});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], json!(["a", "b"]));
        assert_eq!(into["other"], "new");
    }

    #[test]
    fn test_deep_merge_tags_nested_not_set_union() {
        // Tags field that is NOT at top level should NOT get set-union semantics
        let mut into = json!({
            "metadata": {
                "tags": ["a", "b"]
            }
        });
        let from = json!({
            "metadata": {
                "tags": ["c", "d"]
            }
        });
        deep_merge(&mut into, &from);
        // Nested tags should be replaced, not merged
        assert_eq!(into["metadata"]["tags"], json!(["c", "d"]));
    }

    #[test]
    fn test_deep_merge_tags_with_stop_paths() {
        // When tags is in stop_deep_merge_paths, it should be replaced, not set-union merged
        let mut into = json!({
            "tags": ["a", "b"],
            "other": "value"
        });
        let from = json!({
            "tags": ["c", "d"],
            "other": "new_value"
        });
        let mut stop_paths = HashSet::new();
        stop_paths.insert(vec!["tags"]);

        deep_merge_with_stop_paths(&mut into, &from, &stop_paths);

        // Tags should be replaced (stop path), not merged
        assert_eq!(into["tags"], json!(["c", "d"]));
        assert_eq!(into["other"], "new_value");

        // Empty tags array clears tags when tags is in stop paths
        let mut into = json!({"tags": ["a", "b"]});
        let from = json!({"tags": []});
        let mut stop_paths = HashSet::new();
        stop_paths.insert(vec!["tags"]);
        deep_merge_with_stop_paths(&mut into, &from, &stop_paths);
        assert_eq!(into["tags"], json!([]));
    }

    #[test]
    fn test_deep_merge_tags_null_replaces() {
        // null tags should replace existing tags (not subject to set-union)
        let mut into = json!({"tags": ["a", "b"]});
        let from = json!({"tags": null});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], json!(null));
    }

    #[test]
    fn test_deep_merge_tags_non_array_fallback() {
        // If tags is not an array in both, should fall back to replacement
        let mut into = json!({"tags": "not_an_array"});
        let from = json!({"tags": ["a", "b"]});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], json!(["a", "b"]));

        let mut into = json!({"tags": ["a", "b"]});
        let from = json!({"tags": "not_an_array"});
        deep_merge(&mut into, &from);
        assert_eq!(into["tags"], "not_an_array");
    }

    #[test]
    fn test_deep_merge_with_stop_paths() {
        // Test case 1: Basic merge with stop path at shallow level - testing both functions
        {
            // A) Testing deep_merge_with_stop_paths
            let mut into_value = json!({
                "a": {
                    "b": {
                        "c": 1,
                        "d": 2
                    }
                },
                "x": 10
            });

            let from_value = json!({
                "a": {
                    "b": {
                        "c": 3,
                        "e": 4
                    }
                },
                "y": 20
            });

            // Create a stop path for ["a", "b"]
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["a", "b"]);

            deep_merge_with_stop_paths(&mut into_value, &from_value, &stop_paths);

            // The path "a.b" should NOT be deeply merged - it should be completely overwritten
            assert_eq!(
                into_value,
                json!({
                    "a": {
                        "b": {
                            "c": 3,
                            "e": 4
                        }
                    },
                    "x": 10,
                    "y": 20
                })
            );

            // B) Testing deep_merge_maps_with_stop_paths with the same scenario
            let mut into_map = Map::new();
            into_map.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": 1,
                        "d": 2
                    }
                }),
            );
            into_map.insert("x".to_string(), json!(10));

            let mut from_map = Map::new();
            from_map.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": 3,
                        "e": 4
                    }
                }),
            );
            from_map.insert("y".to_string(), json!(20));

            deep_merge_maps_with_stop_paths(&mut into_map, &from_map, &stop_paths);

            // Convert back to Value for comparison
            let map_result = Value::Object(into_map);

            // The results should be identical between both functions
            assert_eq!(map_result, into_value);
        }

        // Test case 2: Multiple stop paths
        {
            let mut into = json!({
                "a": {
                    "b": {
                        "c": 1,
                        "d": 2
                    }
                },
                "x": {
                    "y": {
                        "z": 3
                    }
                }
            });

            let from = json!({
                "a": {
                    "b": {
                        "c": 10,
                        "e": 20
                    }
                },
                "x": {
                    "y": {
                        "z": 30,
                        "w": 40
                    }
                }
            });

            // Create stop paths for ["a", "b"] and ["x"]
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["a", "b"]);
            stop_paths.insert(vec!["x"]);

            deep_merge_with_stop_paths(&mut into, &from, &stop_paths);

            // Both "a.b" and "x" paths should NOT be deeply merged but completely overwritten
            assert_eq!(
                into,
                json!({
                    "a": {
                        "b": {
                            "c": 10,
                            "e": 20
                        }
                    },
                    "x": {
                        "y": {
                            "z": 30,
                            "w": 40
                        }
                    }
                })
            );
        }

        // Test case 3: Stop path with arrays and non-object values
        {
            let mut into = json!({
                "a": [1, 2, 3],
                "b": "hello",
                "c": {
                    "d": [4, 5, 6]
                }
            });

            let from = json!({
                "a": [7, 8, 9],
                "b": "world",
                "c": {
                    "d": [10, 11, 12]
                }
            });

            // Create a stop path for ["c"]
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["c"]);

            deep_merge_with_stop_paths(&mut into, &from, &stop_paths);

            // "c" path should NOT be deeply merged but completely overwritten, and "a" and "b" should also be replaced
            assert_eq!(
                into,
                json!({
                    "a": [7, 8, 9],
                    "b": "world",
                    "c": {
                        "d": [10, 11, 12]
                    }
                })
            );
        }

        // Test case 4: Empty stop paths
        {
            let mut into = json!({
                "a": {
                    "b": {
                        "c": 1
                    }
                }
            });

            let from = json!({
                "a": {
                    "b": {
                        "d": 2
                    }
                }
            });

            // Empty set of stop paths
            let stop_paths = HashSet::new();

            deep_merge_with_stop_paths(&mut into, &from, &stop_paths);

            // Should behave like normal deep_merge
            assert_eq!(
                into,
                json!({
                    "a": {
                        "b": {
                            "c": 1,
                            "d": 2
                        }
                    }
                })
            );
        }

        // Test case 5: Nested path beyond stop path
        {
            let mut into = json!({
                "a": {
                    "b": {
                        "c": {
                            "d": 1
                        }
                    }
                }
            });

            let from = json!({
                "a": {
                    "b": {
                        "c": {
                            "e": 2
                        }
                    }
                }
            });

            // Create a stop path for ["a", "b", "c"]
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["a", "b", "c"]);

            deep_merge_with_stop_paths(&mut into, &from, &stop_paths);

            // The path "a.b.c" should NOT be deeply merged but completely overwritten
            assert_eq!(
                into,
                json!({
                    "a": {
                        "b": {
                            "c": {
                                "e": 2
                            }
                        }
                    }
                })
            );
        }

        // Test case 6: Compare with and without stop paths
        {
            // First with stop paths
            let mut into_map1 = Map::new();
            into_map1.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": 1
                    }
                }),
            );

            let mut from_map = Map::new();
            from_map.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "d": 2
                    }
                }),
            );

            // Create a stop path for ["a", "b"]
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["a", "b"]);

            deep_merge_maps_with_stop_paths(&mut into_map1, &from_map, &stop_paths);

            // Now without stop paths (regular deep merge)
            let mut into_map2 = Map::new();
            into_map2.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": 1
                    }
                }),
            );

            deep_merge_maps_with_stop_paths(&mut into_map2, &from_map, &HashSet::new());

            // Convert to Values for comparison
            let into_value1 = Value::Object(into_map1);
            let into_value2 = Value::Object(into_map2);

            // First should completely overwrite a.b.c
            assert_eq!(
                into_value1,
                json!({
                    "a": {
                        "b": {
                            "d": 2
                        }
                    }
                })
            );

            // Second should merge a.b.d
            assert_eq!(
                into_value2,
                json!({
                    "a": {
                        "b": {
                            "c": 1,
                            "d": 2
                        }
                    }
                })
            );
        }

        // Test case 7: Complex nested structure
        {
            let mut into_map = Map::new();
            into_map.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": {
                            "d": 1,
                            "e": 2
                        },
                        "f": 3
                    },
                    "g": 4
                }),
            );

            let mut from_map = Map::new();
            from_map.insert(
                "a".to_string(),
                json!({
                    "b": {
                        "c": {
                            "d": 10,
                            "h": 20
                        },
                        "f": 30
                    },
                    "g": 40,
                    "i": 50
                }),
            );

            // Create stop paths at different levels
            let mut stop_paths = HashSet::new();
            stop_paths.insert(vec!["a", "b", "c"]); // Stop at a.b.c

            deep_merge_maps_with_stop_paths(&mut into_map, &from_map, &stop_paths);

            // Convert to Value for comparison
            let into_value = Value::Object(into_map);

            // a.b.c should be completely overwritten.
            assert_eq!(
                into_value,
                json!({
                    "a": {
                        "b": {
                            "c": {
                                "d": 10,
                                "h": 20
                            },
                            "f": 30
                        },
                        "g": 40,
                        "i": 50
                    }
                })
            );
        }
    }
}
