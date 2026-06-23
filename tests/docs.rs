//! Documentation guarantees. The Git-state thesis is part of the product, so
//! the README must state it verbatim.

const README: &str = include_str!("../README.md");

#[test]
fn readme_states_the_git_state_thesis() {
    assert!(
        README.contains(
            "We do not ask the model to understand the repo first. \
             We ask Git what state the repo is in."
        ),
        "README.md must contain the Git-state thesis sentence"
    );
}

#[test]
fn readme_has_a_git_starting_point_section() {
    assert!(
        README.contains("## Git as the starting point"),
        "README.md must keep the Git-as-the-starting-point section"
    );
}
