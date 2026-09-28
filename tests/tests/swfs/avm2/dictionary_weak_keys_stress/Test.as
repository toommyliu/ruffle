package {
	import flash.display.Sprite;
	import flash.events.Event;
	import flash.utils.Dictionary;

	public class Test extends Sprite {
		private var seed:uint = 12345;
		private var dictionaries:Array = [];
		private var held:Array = [];
		private var frames:int = 0;
		private var checks:int = 0;
		private var bad:int = 0;

		public function Test() {
			for (var i:int = 0; i < 8; i++) {
				dictionaries.push(new Dictionary(true));
			}
			addEventListener(Event.ENTER_FRAME, onEnterFrame);
		}

		private function random(n:int):int {
			seed = (seed * 1103515245 + 12345) & 0x7fffffff;
			return seed % n;
		}

		private function makeEntry(tag:int):void {
			var dictionary:Dictionary = dictionaries[random(dictionaries.length)];
			var key:Object = {tag: tag};
			var value:Object = {tag: tag, key: key, check: tag * 2};
			switch (random(4)) {
				case 0:
					if (held.length > 0) {
						value.other = held[random(held.length)][1];
					}
					break;
				case 1:
					var nested:Dictionary = new Dictionary(true);
					nested[key] = {tag: tag, key: key};
					value.nested = nested;
					break;
				case 2:
					value.dictionary = dictionaries[random(dictionaries.length)];
					break;
			}
			dictionary[key] = value;
			if (random(3) == 0) {
				held.push([dictionary, key, tag]);
			}
		}

		private function verify(value:Object, key:Object):void {
			checks++;
			if (value == null || value.tag !== key.tag || value.key !== key || value.check !== key.tag * 2) {
				bad++;
				return;
			}
			if (value.nested != null) {
				var inner:Object = value.nested[key];
				if (inner == null || inner.tag !== key.tag || inner.key !== key) {
					bad++;
				}
			}
			if (value.other != null && typeof value.other.tag != "number") {
				bad++;
			}
		}

		private function onEnterFrame(event:Event):void {
			for (var i:int = 0; i < 300; i++) {
				makeEntry(frames * 1000 + i);
			}
			for (i = held.length - 1; i >= 0; i--) {
				if (random(10) == 0) {
					held.splice(i, 1);
				}
			}
			for each (var entry:Array in held) {
				verify(entry[0][entry[1]], entry[1]);
			}
			for each (var dictionary:Dictionary in dictionaries) {
				for (var key:* in dictionary) {
					verify(dictionary[key], key);
				}
			}
			var garbage:Array = [];
			for (i = 0; i < 20000; i++) {
				garbage.push({i: i});
			}

			frames++;
			if (frames == 120) {
				removeEventListener(Event.ENTER_FRAME, onEnterFrame);
				trace("bad values: " + bad);
				trace("checked over " + (checks > 100000 ? "100000" : "only " + checks) + " values");
			}
		}
	}
}
