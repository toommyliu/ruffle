package {
    import flash.display.MovieClip;

    public class Part extends MovieClip {
        public static var throwing:Boolean = false;
        private static var thrown:int = 0;

        public function Part() {
            addFrameScript(0, frame1);
        }

        private function frame1():void {
            if (throwing) {
                thrown++;
                throw new Error("child frame script " + thrown);
            }
        }
    }
}
