import json
from pathlib import Path
import tempfile
import unittest

from scripts import filter_pulse


class PulseFilterTests(unittest.TestCase):
    def test_filters_promotions_and_blocked_hosts_without_removing_real_news(self):
        blocked = [
            {"url": "https://x.com", "title": "AI discussion"},
            {"url": "https://MOBILE.TWITTER.COM:443/user/status/1", "title": "AI discussion"},
            {"url": "https://v2ex.com?tab=ai", "title": "AI discussion"},
            {"url": "https://x.com./user", "title": "AI discussion"},
            {"url": "https://example.com/promo", "title": "[推广] 一个下午验证 Claude 中转，送 10 美元试用金"},
            {"url": "https://example.com/ad", "title": "【广告】AI 产品"},
            {"url": "https://example.com/sponsor", "title": "[Sponsored] AI trial credits"},
            {"url": "https://example.com/translated", "title": "AI trial credits", "title_zh": "[推广] AI 试用金"},
        ]
        allowed = [
            {"url": "https://example.com/news", "title": "AI news"},
            {"url": "https://example.com/industry", "title": "广告业的 AI 转型"},
            {"url": "https://example.com/anthropic", "title": "Anthropic 拒绝 X 公司赞助"},
            {"url": "https://example.com/x.com", "title": "AI discussion on x.com"},
            {"url": "https://examplex.com/news", "title": "AI news"},
        ]
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "feed.json"
            path.write_text(json.dumps({"items": blocked + allowed, "total_items": len(blocked + allowed)}), encoding="utf-8")
            before, after, fixed = filter_pulse.filter_file(path)
            result = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(result["items"], allowed)
            self.assertEqual(result["total_items"], len(allowed))
            self.assertEqual((before, after, fixed), (len(blocked + allowed), len(allowed), 0))


if __name__ == "__main__":
    unittest.main()
