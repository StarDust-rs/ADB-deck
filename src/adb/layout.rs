//! UI hierarchy inspection (`uiautomator dump`).

use std::time::Duration;

use super::DeviceAdb;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    pub fn area(&self) -> i64 {
        i64::from(self.width().max(0)) * i64::from(self.height().max(0))
    }
}

/// `[l,t][r,b]` as written by uiautomator.
pub fn parse_bounds(text: &str) -> Option<Bounds> {
    let nums: Vec<i32> =
        text.split(|c: char| !(c.is_ascii_digit() || c == '-')).filter(|s| !s.is_empty()).map(|s| s.parse().ok()).collect::<Option<_>>()?;
    match nums[..] {
        [left, top, right, bottom] => Some(Bounds { left, top, right, bottom }),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct UiNode {
    pub class: String,
    pub attrs: Vec<(String, String)>,
    pub bounds: Option<Bounds>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl UiNode {
    pub fn attr(&self, name: &str) -> &str {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str()).unwrap_or("")
    }
}

/// Arena of nodes; index 0 is the `hierarchy` root.
#[derive(Debug, Clone, Default)]
pub struct UiTree {
    pub nodes: Vec<UiNode>,
}

pub fn extract_xml(text: &str) -> Option<&str> {
    let start = text.find("<?xml").or_else(|| text.find("<hierarchy"))?;
    let end = text.rfind("</hierarchy>")? + "</hierarchy>".len();
    (end > start).then(|| &text[start..end])
}

impl UiTree {
    pub fn parse(xml: &str) -> Result<UiTree> {
        let doc = roxmltree::Document::parse(xml).map_err(|e| Error::parse("uiautomator dump", e.to_string()))?;
        let mut tree = UiTree::default();
        tree.add(doc.root_element(), None);
        Ok(tree)
    }

    fn add(&mut self, el: roxmltree::Node, parent: Option<usize>) {
        let id = self.nodes.len();
        let attrs: Vec<(String, String)> = el.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect();
        let class = el.attribute("class").map(str::to_string).unwrap_or_else(|| el.tag_name().name().to_string());
        let bounds = el.attribute("bounds").and_then(parse_bounds);
        self.nodes.push(UiNode { class, attrs, bounds, parent, children: Vec::new() });
        if let Some(p) = parent {
            self.nodes[p].children.push(id);
        }
        for child in el.children().filter(roxmltree::Node::is_element) {
            self.add(child, Some(id));
        }
    }

    /// Deepest/smallest node under a screen point; later siblings (drawn on
    /// top) win ties.
    pub fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.bounds.is_some_and(|b| b.contains(x, y)))
            .min_by_key(|(i, n)| (n.bounds.map(|b| b.area()).unwrap_or(i64::MAX), std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
    }

    pub fn ancestors(&self, mut id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(p) = self.nodes[id].parent {
            out.push(p);
            id = p;
        }
        out
    }

    /// XPath in the style used by Appium tools: a unique resource-id when
    /// there is one, otherwise an indexed path of class names.
    pub fn xpath(&self, id: usize) -> String {
        let node = &self.nodes[id];
        let rid = node.attr("resource-id");
        if !rid.is_empty() && self.nodes.iter().filter(|n| n.attr("resource-id") == rid).count() == 1 {
            return format!("//*[@resource-id=\"{rid}\"]");
        }
        let mut segments = Vec::new();
        let mut cur = id;
        while let Some(parent) = self.nodes[cur].parent {
            let class = &self.nodes[cur].class;
            let same: Vec<usize> = self.nodes[parent].children.iter().copied().filter(|c| self.nodes[*c].class == *class).collect();
            let pos = same.iter().position(|c| *c == cur).unwrap_or(0) + 1;
            segments.push(if same.len() > 1 { format!("{class}[{pos}]") } else { class.clone() });
            cur = parent;
        }
        segments.push(self.nodes[0].class.clone());
        segments.reverse();
        format!("/{}", segments.join("/"))
    }

    /// Summary used for tree rows: class short name plus text/id hints.
    pub fn label(&self, id: usize) -> String {
        let n = &self.nodes[id];
        let short = n.class.rsplit('.').next().unwrap_or(&n.class);
        let text = n.attr("text");
        let rid = n.attr("resource-id").rsplit('/').next().unwrap_or("");
        match (text.is_empty(), rid.is_empty()) {
            (false, false) => format!("{short} #{rid} \"{text}\""),
            (false, true) => format!("{short} \"{text}\""),
            (true, false) => format!("{short} #{rid}"),
            (true, true) => short.to_string(),
        }
    }
}

impl DeviceAdb {
    /// Dumps the current UI hierarchy as XML text.
    pub async fn dump_hierarchy(&self) -> Result<String> {
        let out = self.exec_out(&["uiautomator", "dump", "/dev/tty"], 32 * 1024 * 1024, Duration::from_secs(30)).await?;
        let text = out.stdout_text();
        if let Some(xml) = extract_xml(&text) {
            return Ok(xml.to_string());
        }
        let msg = text.trim();
        Err(Error::Invalid(if msg.is_empty() {
            "The device returned no UI hierarchy. Make sure the screen is on and unlocked.".into()
        } else {
            format!("Could not read the UI hierarchy: {msg}")
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n<hierarchy rotation=\"0\">\
<node index=\"0\" text=\"\" resource-id=\"\" class=\"android.widget.FrameLayout\" bounds=\"[0,0][1080,2400]\">\
<node index=\"0\" text=\"Hello\" resource-id=\"com.x:id/title\" class=\"android.widget.TextView\" bounds=\"[100,200][500,300]\"/>\
<node index=\"1\" text=\"\" resource-id=\"\" class=\"android.widget.Button\" bounds=\"[100,400][500,500]\"/>\
<node index=\"2\" text=\"\" resource-id=\"\" class=\"android.widget.Button\" bounds=\"[100,600][500,700]\"/>\
</node></hierarchy>UI hierchary dumped to: /dev/tty";

    #[test]
    fn extracts_xml_from_command_output() {
        let xml = extract_xml(XML).unwrap();
        assert!(xml.ends_with("</hierarchy>"));
        assert!(extract_xml("ERROR: null root node returned by UiTestAutomationBridge.").is_none());
    }

    #[test]
    fn builds_tree_with_bounds() {
        let tree = UiTree::parse(extract_xml(XML).unwrap()).unwrap();
        assert_eq!(tree.nodes.len(), 5);
        assert_eq!(tree.nodes[0].class, "hierarchy");
        assert_eq!(tree.nodes[2].bounds, Some(Bounds { left: 100, top: 200, right: 500, bottom: 300 }));
        assert_eq!(tree.nodes[1].children.len(), 3);
        assert_eq!(tree.ancestors(2), vec![1, 0]);
    }

    #[test]
    fn hit_test_prefers_the_smallest_containing_node() {
        let tree = UiTree::parse(extract_xml(XML).unwrap()).unwrap();
        assert_eq!(tree.hit_test(200, 250), Some(2));
        assert_eq!(tree.hit_test(900, 1500), Some(1));
        assert_eq!(tree.hit_test(5000, 5000), None);
    }

    #[test]
    fn xpath_uses_unique_ids_or_indexed_paths() {
        let tree = UiTree::parse(extract_xml(XML).unwrap()).unwrap();
        assert_eq!(tree.xpath(2), "//*[@resource-id=\"com.x:id/title\"]");
        assert_eq!(tree.xpath(4), "/hierarchy/android.widget.FrameLayout/android.widget.Button[2]");
    }

    #[test]
    fn labels_are_short_and_informative() {
        let tree = UiTree::parse(extract_xml(XML).unwrap()).unwrap();
        assert_eq!(tree.label(2), "TextView #title \"Hello\"");
        assert_eq!(tree.label(3), "Button");
    }

    #[test]
    fn bounds_parsing() {
        assert_eq!(parse_bounds("[0,0][10,20]"), Some(Bounds { left: 0, top: 0, right: 10, bottom: 20 }));
        assert_eq!(parse_bounds("[1,2][3]"), None);
        assert_eq!(parse_bounds(""), None);
    }

    #[test]
    fn malformed_xml_is_a_parse_error() {
        assert!(matches!(UiTree::parse("<hierarchy><node></hierarchy>"), Err(Error::Parse { .. })));
    }
}
