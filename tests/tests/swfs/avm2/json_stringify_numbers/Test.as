package {
    import flash.display.MovieClip;
    import flash.external.ExternalInterface;

    public class Test extends MovieClip {
        public function Test() {
            var negativeZero:Number = 1 / -Infinity;
            out(JSON.stringify([0, negativeZero, 1, -1, 2.5, 0.1, 1e-7, 1.5e-10, 268435455, 268435456, -268435457, int.MAX_VALUE, int.MIN_VALUE, uint.MAX_VALUE, 4294967296, 1e10, 123456789012, 1727740000000, 1e15, 1e16, 1e21, -1.5e300, NaN, Infinity, -Infinity]));
            out(JSON.stringify({n: 1e10}, null, 1));
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
