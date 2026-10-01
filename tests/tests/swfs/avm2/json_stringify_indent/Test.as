package {
    import flash.display.MovieClip;
    import flash.external.ExternalInterface;

    public class Test extends MovieClip {
        public function Test() {
            var value:Array = [1, "s", null, true, [], {}, [[]], [{}], {a: []}, {b: {}}, {c: [1, {d: 2.5}]}];
            out(JSON.stringify(value, null, 3));
            out(JSON.stringify(value, null, "\t"));
            out(JSON.stringify(value, null, "abcdefghijklmnop"));
            out(JSON.stringify(value, null, 20));
            out(JSON.stringify(value, null, 0));
            out(JSON.stringify({a: 1, b: 2}, ["a", "b", "a"], 1));
            if (ExternalInterface.available) {
                ExternalInterface.call("report", "done");
            }
        }

        private static function out(result:String):void {
            trace(result);
            if (ExternalInterface.available) {
                for each (var line:String in result.split("\n")) {
                    ExternalInterface.call("report", line);
                }
            }
        }
    }
}
