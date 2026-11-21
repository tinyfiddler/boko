//! Pass 6: Pruner (Empty Container Removal)

use crate::model::{Chapter, NodeId};
use crate::style::{Length, StyleId};

use super::pass::walk_bottom_up;
use super::predicates::{has_semantic_attrs, is_prunable_role};

/// Remove empty containers in post-order (cascading).
///
/// "Div soup" from old converters leaves empty containers:
/// ```html
/// <div class="clear"></div>
/// <span id="ad-placeholder"></span>
/// ```
///
/// Post-order traversal enables cascading:
/// - `<div><span></span></div>`
/// - Step 1: Visit span, it's empty, delete
/// - Step 2: Visit div, now empty, delete
/// - Result: Entire dead subtree vanishes
pub fn prune_empty(chapter: &mut Chapter) {
    walk_bottom_up(chapter, |chapter, parent_id| {
        prune_siblings(chapter, parent_id);
    });
}

fn prune_siblings(chapter: &mut Chapter, parent_id: NodeId) {
    let mut prev_opt: Option<NodeId> = None;
    let mut cursor_opt = chapter.node(parent_id).and_then(|n| n.first_child);

    while let Some(current_id) = cursor_opt {
        let next_opt = chapter.node(current_id).and_then(|n| n.next_sibling);

        if should_prune(chapter, current_id) {
            // Unlink this node
            if let Some(prev_id) = prev_opt {
                if let Some(prev_node) = chapter.node_mut(prev_id) {
                    prev_node.next_sibling = next_opt;
                }
            } else if let Some(parent_node) = chapter.node_mut(parent_id) {
                parent_node.first_child = next_opt;
            }
            // Don't update prev_opt
        } else {
            prev_opt = Some(current_id);
        }

        cursor_opt = next_opt;
    }
}

/// Check if a node should be pruned (empty container).
fn should_prune(chapter: &Chapter, node_id: NodeId) -> bool {
    let node = match chapter.node(node_id) {
        Some(n) => n,
        None => return false,
    };

    // Only prune containers (never Text, Image, Break, Rule)
    if !is_prunable_role(node.role) {
        return false;
    }

    // Must have no children
    if node.first_child.is_some() {
        return false;
    }

    // Must have no text content
    if !node.text.is_empty() {
        return false;
    }

    // Safety: Don't prune anything carrying semantic attributes. This is the
    // same guard merge/fuse use — an id might be a link target, and an empty
    // `<span epub:type="pagebreak" title="23"/>` page marker is meaningful
    // content even without one.
    if has_semantic_attrs(chapter, node_id) {
        return false;
    }

    !holds_height(chapter, node.style)
}

/// Books push a title down the page with an empty element of fixed height.
/// A percentage height of an auto-height parent computes to `auto`, and an
/// absolutely positioned box holds no room in the flow.
fn holds_height(chapter: &Chapter, style: StyleId) -> bool {
    let fixed = |len: Length| match len {
        Length::Px(v) | Length::Em(v) | Length::Rem(v) => v > 0.0,
        Length::Auto | Length::Percent(_) => false,
    };
    chapter
        .styles
        .get(style)
        .is_some_and(|s| !s.position.is_out_of_flow() && (fixed(s.height) || fixed(s.min_height)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, Role};

    #[test]
    fn test_prunes_empty_container() {
        let mut chapter = Chapter::new();

        let container = chapter.alloc_node(Node::new(Role::Container));
        chapter.append_child(NodeId::ROOT, container);

        prune_empty(&mut chapter);

        assert_eq!(chapter.children(NodeId::ROOT).count(), 0);
    }

    #[test]
    fn test_prune_cascades() {
        let mut chapter = Chapter::new();

        let container = chapter.alloc_node(Node::new(Role::Container));
        chapter.append_child(NodeId::ROOT, container);

        let inline = chapter.alloc_node(Node::new(Role::Inline));
        chapter.append_child(container, inline);

        assert_eq!(chapter.children(container).count(), 1);

        prune_empty(&mut chapter);

        assert_eq!(chapter.children(NodeId::ROOT).count(), 0);
    }

    #[test]
    fn test_preserves_empty_container_holding_a_height() {
        use crate::style::{ComputedStyle, Length, Position};

        let mut chapter = Chapter::new();
        let cases = [
            (Length::Em(3.0), Length::Auto, Position::Static, true),
            (Length::Auto, Length::Px(32.0), Position::Relative, true),
            (
                Length::Percent(100.0),
                Length::Auto,
                Position::Static,
                false,
            ),
            (Length::Em(0.0), Length::Auto, Position::Static, false),
            (Length::Px(648.0), Length::Auto, Position::Absolute, false),
            (Length::Px(648.0), Length::Auto, Position::Fixed, false),
        ];
        let mut kept = Vec::new();
        for (height, min_height, position, keep) in cases {
            let style = chapter.styles.intern(ComputedStyle {
                height,
                min_height,
                position,
                ..Default::default()
            });
            let container = chapter.alloc_node(Node::new(Role::Container));
            if let Some(node) = chapter.node_mut(container) {
                node.style = style;
            }
            chapter.append_child(NodeId::ROOT, container);
            if keep {
                kept.push(container);
            }
        }

        prune_empty(&mut chapter);

        assert_eq!(chapter.children(NodeId::ROOT).collect::<Vec<_>>(), kept);
    }

    #[test]
    fn test_preserves_node_with_id() {
        let mut chapter = Chapter::new();

        let container = chapter.alloc_node(Node::new(Role::Container));
        chapter.append_child(NodeId::ROOT, container);
        chapter.semantics.set_id(container, "anchor");

        prune_empty(&mut chapter);

        assert_eq!(chapter.children(NodeId::ROOT).count(), 1);
    }

    #[test]
    fn test_preserves_node_with_content() {
        let mut chapter = Chapter::new();

        let container = chapter.alloc_node(Node::new(Role::Container));
        chapter.append_child(NodeId::ROOT, container);

        let text_range = chapter.append_text("Content");
        let text = chapter.alloc_node(Node::text(text_range));
        chapter.append_child(container, text);

        prune_empty(&mut chapter);

        assert_eq!(chapter.children(NodeId::ROOT).count(), 1);
    }
}
